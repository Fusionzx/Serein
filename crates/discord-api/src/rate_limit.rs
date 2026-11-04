//! Per-route REST admission based on Discord's rate-limit response headers.
//!
//! Route and bucket behavior is service controlled. Unknown routes remain available until a
//! response teaches us a bucket; all tracking is session-local and bounded.

use reqwest::{Method, header::HeaderMap};
use serde_json::Value;
use std::{
	collections::HashMap,
	sync::Mutex,
	time::{Duration, Instant},
};

const MAX_TRACKED_ROUTES: usize = 4096;
const MAX_RESET: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_WAIT: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Limits {
	state: Mutex<State>,
}

#[derive(Default)]
struct State {
	route_buckets: HashMap<String, String>,
	buckets: HashMap<String, Bucket>,
	global_until: Option<Instant>,
}

struct Bucket {
	remaining: u64,
	reset: Instant,
}

impl Limits {
	/// Wait as Yapfast does for the route bucket Discord last reported.
	pub(crate) async fn acquire(&self, route: &str, speculative: bool) -> Result<(), Duration> {
		loop {
			let wait = self.reserve(route, Instant::now());
			if wait.is_zero() {
				return Ok(());
			}
			if speculative || wait > MAX_WAIT {
				return Err(wait);
			}
			tokio::time::sleep(wait).await;
		}
	}

	/// Return an admission if a speculative request cannot obtain spare REST capacity.
	pub(crate) fn refund(&self, route: &str) {
		let mut state = self.lock();
		let Some(key) = state.route_buckets.get(route).cloned() else {
			return;
		};
		if let Some(bucket) = state.buckets.get_mut(&key)
			&& bucket.reset > Instant::now()
		{
			bucket.remaining = bucket.remaining.saturating_add(1);
		}
	}

	/// Record a normal response. Discord supplies the bucket identity and its remaining window.
	pub(crate) fn update(&self, route: &str, headers: &HeaderMap) {
		let bucket = headers
			.get("x-ratelimit-bucket")
			.and_then(|value| value.to_str().ok());
		let Some(remaining) = header_u64(headers, "x-ratelimit-remaining") else {
			return;
		};
		let Some(reset) = header_duration(headers, "x-ratelimit-reset-after") else {
			return;
		};
		let mut state = self.lock();
		let key = Self::key(&mut state, route, bucket);
		state.buckets.insert(
			key,
			Bucket {
				remaining,
				reset: Instant::now() + reset,
			},
		);
	}

	/// Learn a 429 wait from the response, using Yapfast's body/header precedence and fallback.
	pub(crate) fn limited(&self, route: &str, headers: &HeaderMap, body: &Value) -> Duration {
		let retry_after = body
			.get("retry_after")
			.and_then(Value::as_f64)
			.and_then(bounded_seconds);
		let retry_header = header_duration(headers, "retry-after");
		let reset = header_duration(headers, "x-ratelimit-reset-after");
		let delay = retry_after
			.into_iter()
			.chain(retry_header)
			.chain(reset)
			.max()
			.unwrap_or(Duration::from_secs(1));
		let global = body.get("global").and_then(Value::as_bool) == Some(true)
			|| headers
				.get("x-ratelimit-global")
				.and_then(|value| value.to_str().ok())
				.is_some_and(|value| value.eq_ignore_ascii_case("true"))
			|| headers
				.get("x-ratelimit-scope")
				.and_then(|value| value.to_str().ok())
				.is_some_and(|value| value == "global");
		let bucket = headers
			.get("x-ratelimit-bucket")
			.and_then(|value| value.to_str().ok());
		let mut state = self.lock();
		let until = Instant::now() + delay.min(MAX_RESET);
		if global {
			state.global_until = Some(state.global_until.map_or(until, |old| old.max(until)));
			return delay;
		}
		let key = Self::key(&mut state, route, bucket);
		state.buckets.insert(
			key,
			Bucket {
				remaining: 0,
				reset: until,
			},
		);
		delay
	}

	fn reserve(&self, route: &str, now: Instant) -> Duration {
		let mut state = self.lock();
		if let Some(until) = state.global_until {
			if until > now {
				return until - now;
			}
			state.global_until = None;
		}
		let Some(key) = state.route_buckets.get(route).cloned() else {
			return Duration::ZERO;
		};
		let Some(bucket) = state.buckets.get_mut(&key) else {
			return Duration::ZERO;
		};
		if bucket.reset <= now {
			state.buckets.remove(&key);
			return Duration::ZERO;
		}
		if bucket.remaining == 0 {
			return bucket.reset - now;
		}
		bucket.remaining -= 1;
		Duration::ZERO
	}

	fn key(state: &mut State, route: &str, bucket: Option<&str>) -> String {
		let key = bucket.map_or_else(
			|| {
				state
					.route_buckets
					.get(route)
					.cloned()
					.unwrap_or_else(|| route.to_owned())
			},
			|bucket| format!("{bucket}{}", major_parameters(route)),
		);
		if !state.route_buckets.contains_key(route)
			&& state.route_buckets.len() >= MAX_TRACKED_ROUTES
		{
			state.route_buckets.clear();
			state.buckets.clear();
		}
		state.route_buckets.insert(route.to_owned(), key.clone());
		key
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, State> {
		self.state
			.lock()
			.unwrap_or_else(std::sync::PoisonError::into_inner)
	}
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
	headers.get(name)?.to_str().ok()?.parse().ok()
}

fn header_duration(headers: &HeaderMap, name: &str) -> Option<Duration> {
	let seconds: f64 = headers.get(name)?.to_str().ok()?.parse().ok()?;
	bounded_seconds(seconds)
}

fn bounded_seconds(seconds: f64) -> Option<Duration> {
	(seconds.is_finite() && seconds >= 0.0)
		.then(|| Duration::from_secs_f64(seconds.min(MAX_RESET.as_secs_f64())))
}

/// Replace changing IDs but retain the major channel, guild, or webhook ID for bucket scope.
pub(crate) fn route(method: &Method, path: &str) -> String {
	let path = path.split_once('?').map_or(path, |(path, _)| path);
	let segments: Vec<_> = path.trim_start_matches('/').split('/').collect();
	let mut previous = "";
	let mut normalized = String::new();
	for (index, segment) in segments.iter().copied().enumerate() {
		let is_major =
			index > 0 && index == 1 && matches!(previous, "channels" | "guilds" | "webhooks");
		let is_webhook_token = index == 2 && segments.first() == Some(&"webhooks");
		let replacement = if is_major {
			segment
		} else if is_webhook_token {
			":token"
		} else if previous == "reactions" {
			":emoji"
		} else if segment.bytes().all(|byte| byte.is_ascii_digit()) {
			":id"
		} else {
			segment
		};
		normalized.push('/');
		normalized.push_str(replacement);
		previous = segment;
	}
	format!("{} {normalized}", method.as_str())
}

fn major_parameters(route: &str) -> String {
	let mut major = String::new();
	let mut segments = route.split('/').peekable();
	while let Some(segment) = segments.next() {
		if matches!(segment, "channels" | "guilds" | "webhooks")
			&& let Some(id) = segments.peek()
		{
			major.push('/');
			major.push_str(segment);
			major.push('/');
			major.push_str(id);
		}
	}
	major
}

#[cfg(test)]
mod tests {
	use super::*;
	use reqwest::header::HeaderValue;
	use serde_json::json;

	fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
		let mut headers = HeaderMap::new();
		for (name, value) in pairs {
			headers.insert(*name, HeaderValue::from_str(value).unwrap());
		}
		headers
	}

	#[test]
	fn route_keys_keep_major_ids_and_normalize_message_ids() {
		assert_eq!(
			route(&Method::GET, "/channels/12/messages/34?limit=50"),
			"GET /channels/12/messages/:id"
		);
		assert_eq!(
			route(&Method::GET, "/users/56/profile"),
			"GET /users/:id/profile"
		);
		assert_eq!(
			route(&Method::POST, "/webhooks/12/private-token?wait=true"),
			"POST /webhooks/12/:token"
		);
	}

	#[test]
	fn learned_buckets_are_scoped_by_major_resource_and_global_429_is_separate() {
		let limits = Limits::default();
		let first = "POST /channels/1/messages";
		let second = "POST /channels/2/messages";
		let response = headers(&[
			("x-ratelimit-bucket", "messages"),
			("x-ratelimit-remaining", "1"),
			("x-ratelimit-reset-after", "2"),
		]);
		limits.update(first, &response);
		assert_eq!(limits.reserve(first, Instant::now()), Duration::ZERO);
		assert!(limits.reserve(first, Instant::now()) > Duration::from_secs(1));
		assert_eq!(limits.reserve(second, Instant::now()), Duration::ZERO);

		limits.limited(
			first,
			&HeaderMap::new(),
			&json!({"global": false, "retry_after": 2.0}),
		);
		assert!(limits.reserve(first, Instant::now()) > Duration::from_secs(1));
		assert_eq!(limits.reserve(second, Instant::now()), Duration::ZERO);
		limits.limited(
			first,
			&HeaderMap::new(),
			&json!({"global": true, "retry_after": 2.0}),
		);
		assert!(limits.reserve(second, Instant::now()) > Duration::from_secs(1));
	}

	#[test]
	fn malformed_rate_limit_headers_are_ignored_and_route_state_is_bounded() {
		let limits = Limits::default();
		limits.update(
			"GET /users/@me",
			&headers(&[
				("x-ratelimit-remaining", "not-a-number"),
				("x-ratelimit-reset-after", "NaN"),
			]),
		);
		assert_eq!(
			limits.reserve("GET /users/@me", Instant::now()),
			Duration::ZERO
		);
		for id in 0..=MAX_TRACKED_ROUTES {
			limits.update(
				&format!("GET /channels/{id}/messages"),
				&headers(&[
					("x-ratelimit-bucket", "bucket"),
					("x-ratelimit-remaining", "1"),
					("x-ratelimit-reset-after", "2"),
				]),
			);
		}
		assert!(limits.lock().route_buckets.len() <= MAX_TRACKED_ROUTES);
	}
}
