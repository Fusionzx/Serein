//! Native embed cards. External links share the timeline's explicit confirmation.
use crate::{
	attachments::{DownloadUi, embed_context_menu},
	avatars::{Avatars, Surface},
	markdown::{FormatCache, external_url},
};
use egui::RichText;
use model::{Embed, Gif, Message};
use std::borrow::Cow;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::OnceLock;

pub fn has_spoilers(message: &Message) -> bool {
	has_media_spoilers(message) || message.content.contains("||")
}
pub fn has_media_spoilers(message: &Message) -> bool {
	message.attachments.iter().any(|a| {
		a.spoiler
			|| a.filename.starts_with("SPOILER_")
			|| a.description.as_deref().is_some_and(|s| s.contains("||"))
	}) || message.embeds.iter().any(|e| {
		[&e.title, &e.description]
			.into_iter()
			.flatten()
			.any(|s| s.contains("||"))
			|| e.fields
				.iter()
				.any(|f| f.name.contains("||") || f.value.contains("||"))
			|| [&e.author, &e.provider]
				.into_iter()
				.flatten()
				.any(|a| a.name.contains("||"))
			|| e.footer.as_ref().is_some_and(|f| f.text.contains("||"))
	})
}
fn link(
	ui: &mut egui::Ui,
	label: &str,
	url: Option<&str>,
	opening: &mut Option<String>,
	strong: bool,
) {
	let target = url.and_then(external_url);
	let mut text = RichText::new(label);
	if strong {
		text = text.strong();
	}
	if target.is_some() {
		text = text.color(ui.visuals().hyperlink_color);
	}
	let response = ui.add(egui::Label::new(text).wrap().sense(if target.is_some() {
		egui::Sense::click()
	} else {
		egui::Sense::hover()
	}));
	if let Some(target) = target {
		response
			.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Link, ui.is_enabled(), label));
		if response
			.on_hover_text(crate::i18n::translate("embeds-link-open-link"))
			.clicked()
		{
			*opening = Some(target);
		}
	}
}

/// A source action is needed only when the card and message have no existing source link.
fn source_fallback(
	message: &Message,
	embed: &Embed,
	cache: &mut FormatCache,
	message_links_shown: bool,
) -> Option<String> {
	let inline = inline_image(embed).is_some();
	if (!inline
		&& embed
			.title
			.as_deref()
			.is_some_and(|title| !title.is_empty()))
		|| embed.video.is_some()
		|| matches!(embed.kind.as_str(), "video" | "gifv")
	{
		return None;
	}
	let url = embed.url.as_deref().and_then(external_url)?;
	if (message_links_shown && cache.get(message.id, &message.content).links.contains(&url))
		|| (!inline
			&& [&embed.author, &embed.provider]
				.into_iter()
				.flatten()
				.any(|author| {
					!author.name.is_empty()
						&& author.url.as_deref().and_then(external_url).as_ref() == Some(&url)
				})) {
		return None;
	}
	Some(url)
}
#[allow(clippy::too_many_arguments)]
fn text(
	ui: &mut egui::Ui,
	message: &Message,
	part: (u16, &str),
	cache: &mut FormatCache,
	opening: &mut Option<String>,
	profile: &mut crate::profiles::ProfileSession,
	card_surface: crate::design::MessageCardSurface,
	media: (
		&mut Avatars,
		bool,
		&[model::Guild],
		&crate::mentions::MentionSource<'_>,
	),
) -> egui::Response {
	let (images, demo, guilds, source) = media;
	let preferred_guild = source
		.state
		.channel(source.channel)
		.and_then(|channel| channel.guild);
	let expanded = expand_shortcodes(part.1, guilds, preferred_guild, message, source);
	let formatted = cache.get_part(message.id, part.0, &expanded);
	ui.scope(|ui| {
		formatted.show_with_images(
			ui,
			opening,
			&message.mentions,
			Some(source),
			profile,
			(images, demo, guilds),
			card_surface,
		);
		if formatted.limited {
			ui.small(crate::i18n::translate("embeds-text-text-display-limited"));
		}
	})
	.response
}

/// Message bodies can contain emoji names instead of wire emoji tokens. Resolve those names
/// using only the message's server, while leaving code spans and unknown names untouched.
pub(crate) fn expanded_message_content<'a>(
	message: &'a Message,
	state: &client_core::State,
) -> Cow<'a, str> {
	if !message.content.contains(':') {
		return Cow::Borrowed(&message.content);
	}
	let source = crate::mentions::MentionSource {
		state,
		channel: message.channel,
	};
	let preferred_guild = state
		.channel(message.channel)
		.and_then(|channel| channel.guild);
	Cow::Owned(expand_shortcodes(
		&message.content,
		&state.guilds,
		preferred_guild,
		message,
		&source,
	))
}

/// Discord's embed text may contain Unicode emoji names even when the wire text is literal.
/// Leave code spans and fenced examples alone.
fn expand_shortcodes(
	source: &str,
	guilds: &[model::Guild],
	preferred_guild: Option<model::Id>,
	message: &Message,
	mention_source: &crate::mentions::MentionSource<'_>,
) -> String {
	static NAMES: OnceLock<std::collections::HashMap<String, &'static str>> = OnceLock::new();
	let names = NAMES.get_or_init(|| {
		let mut names = std::collections::HashMap::new();
		for ((emoji, _), (_, _, aliases)) in crate::emoji_picker::standard()
			.iter()
			.zip(crate::emoji_picker::discord_names())
		{
			for alias in aliases.split(',') {
				names.insert(alias.to_ascii_lowercase(), *emoji);
			}
		}
		names
	});
	let mut output = String::with_capacity(source.len());
	let mut offset = 0;
	let mut fence = false;
	let mut inline = false;
	while offset < source.len() {
		let tail = &source[offset..];
		// Preserve complete Discord emoji and mention tokens. In particular, the numeric
		// suffix of <:name:id> is an emoji ID, never a user mention candidate.
		if !fence && let Some((_, len)) = model::user_mention_prefix(tail) {
			output.push_str(&tail[..len]);
			offset += len;
			continue;
		}
		if !fence && let Some((_, len)) = model::role_mention_prefix(tail) {
			output.push_str(&tail[..len]);
			offset += len;
			continue;
		}
		if !fence && let Some((_, len)) = model::channel_mention_prefix(tail) {
			output.push_str(&tail[..len]);
			offset += len;
			continue;
		}
		if !fence && let Some((_, len)) = crate::emoji::custom_prefix(tail) {
			output.push_str(&tail[..len]);
			offset += len;
			continue;
		}
		let has_at = tail.starts_with('@');
		let digits_start = usize::from(has_at);
		if !fence
			&& !inline
			&& (has_at
				|| offset == 0
				|| !source[..offset]
					.chars()
					.next_back()
					.is_some_and(|c| c.is_alphanumeric() || c == '_'))
			&& let digit_count = tail[digits_start..]
				.bytes()
				.take_while(u8::is_ascii_digit)
				.count() && (18..=20).contains(&digit_count)
			&& let Some(end) = digits_start.checked_add(digit_count)
			&& tail
				.as_bytes()
				.get(end)
				.is_none_or(|byte| !byte.is_ascii_digit())
			&& let Ok(id) = tail[digits_start..end].parse::<u64>()
			&& crate::mentions::find_user(model::Id(id), &message.mentions, Some(mention_source))
				.is_some()
		{
			output.push_str("<@");
			output.push_str(&tail[digits_start..end]);
			output.push('>');
			let wrapped_close = !has_at
				&& offset > 0
				&& source[..offset].ends_with('>')
				&& tail[end..].starts_with('>');
			offset += end + usize::from(wrapped_close);
			continue;
		}
		if let Some(after_fence) = tail.strip_prefix("```") {
			if fence {
				fence = false;
			} else if after_fence.contains("```") {
				// Only a paired fence protects the rest of the description. A stray
				// triple-backtick must not disable shortcode expansion to end of embed.
				fence = true;
			}
			output.push_str("```");
			offset += 3;
			continue;
		}
		if tail.starts_with('`') && !fence {
			if inline || tail[1..].contains('`') {
				inline = !inline;
			}
			output.push('`');
			offset += 1;
			continue;
		}
		if !fence
			&& !inline
			&& tail.starts_with(':')
			&& let Some(end) = tail[1..].find(':').map(|end| end + 2)
			&& (3..=66).contains(&end)
			&& tail[1..end - 1]
				.bytes()
				.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'+' | b'-'))
		{
			let name = &tail[1..end - 1];
			let custom = preferred_guild
				.and_then(|guild| guilds.iter().find(|entry| entry.id == guild))
				.and_then(|guild| guild.emojis.as_deref())
				.and_then(|emojis| {
					emojis
						.iter()
						.find(|emoji| emoji.name.eq_ignore_ascii_case(name))
				});
			if let Some(custom) = custom {
				output.push_str(if custom.animated { "<a:" } else { "<:" });
				output.push_str(&custom.name);
				output.push(':');
				output.push_str(&custom.id.to_string());
				output.push('>');
			} else if let Some(emoji) = names.get(&name.to_ascii_lowercase()) {
				output.push_str(emoji);
			} else if let Some(flag) = country_flag_shortcode(name) {
				output.push_str(&flag);
			} else {
				output.push_str(&tail[..end]);
			}
			offset += end;
			continue;
		}
		let character = tail.chars().next().expect("nonempty tail");
		output.push(character);
		offset += character.len_utf8();
	}
	output
}

fn country_flag_shortcode(name: &str) -> Option<String> {
	let code = name.strip_prefix("flag_")?;
	let [first, second] = code.as_bytes() else {
		return None;
	};
	if !first.is_ascii_alphabetic() || !second.is_ascii_alphabetic() {
		return None;
	}
	Some(
		[
			0x1f1e6 + u32::from(first.to_ascii_uppercase() - b'A'),
			0x1f1e6 + u32::from(second.to_ascii_uppercase() - b'A'),
		]
		.into_iter()
		.filter_map(char::from_u32)
		.collect(),
	)
}

#[allow(clippy::too_many_arguments)]
fn rich_text(
	ui: &mut egui::Ui,
	message: &Message,
	part: u16,
	value: &str,
	strong: bool,
	color: Option<egui::Color32>,
	target: Option<&str>,
	cache: &mut FormatCache,
	opening: &mut Option<String>,
	profile: &mut crate::profiles::ProfileSession,
	card_surface: crate::design::MessageCardSurface,
	images: &mut Avatars,
	demo: bool,
	state: &client_core::State,
) -> egui::Response {
	let source = crate::mentions::MentionSource {
		state,
		channel: message.channel,
	};
	let value = if strong {
		format!("**{value}**")
	} else {
		value.to_owned()
	};
	let rendered = ui
		.scope(|ui| {
			if let Some(color) = color {
				ui.visuals_mut().override_text_color = Some(color);
			}
			text(
				ui,
				message,
				(part, &value),
				cache,
				opening,
				profile,
				card_surface,
				(images, demo, &state.guilds, &source),
			)
		})
		.response;
	let Some(target) = target.and_then(external_url) else {
		return rendered;
	};
	let response = ui
		.interact(
			rendered.rect,
			rendered.id.with(("embed-rich-link", part)),
			egui::Sense::click(),
		)
		.on_hover_cursor(egui::CursorIcon::PointingHand);
	if response.clicked() {
		*opening = Some(target);
	}
	response
}
pub fn standalone_media_links(message: &Message) -> bool {
	!message.embeds_suppressed
		&& !has_spoilers(message)
		&& !message.content.trim().is_empty()
		&& message.content.split_whitespace().all(|link| {
			message.embeds.iter().any(|embed| {
				inline_image(embed).is_some()
					&& (embed.url.as_deref() == Some(link)
						|| [&embed.image, &embed.thumbnail]
							.into_iter()
							.flatten()
							.any(|media| media.url.as_deref() == Some(link)))
			})
		})
}

fn inline_image(embed: &Embed) -> Option<&model::EmbedMedia> {
	matches!(embed.kind.as_str(), "image" | "gifv")
		.then(|| {
			embed
				.image
				.as_ref()
				.or(embed.thumbnail.as_ref())
				.or_else(|| {
					(embed.kind == "gifv")
						.then_some(embed.video.as_ref())
						.flatten()
				})
		})
		.flatten()
}

/// A link that resolves to a hosted clip arrives as a video embed without a poster
/// image. Discord draws the clip itself there, so the card shows its frames instead of
/// an empty page; playback still opens in the browser through the card's video line.
fn clip_media(embed: &Embed) -> Option<&model::EmbedMedia> {
	let clip = embed.video.as_ref()?;
	(embed.image.is_none()
		&& embed.thumbnail.is_none()
		&& (clip.url.is_some() || clip.proxy_url.is_some()))
	.then_some(clip)
}

// Discord link previews carry additional images as same-URL embed entries.
// Only consume continuations without independent content (or with repeated metadata).
fn gallery_len(embeds: &[Embed]) -> usize {
	let Some(first) = embeds.first() else {
		return 0;
	};
	let eligible = |e: &Embed| {
		e.image.is_some()
			&& e.video.is_none()
			&& matches!(e.kind.as_str(), "rich" | "article" | "link" | "image")
	};
	if !eligible(first) || first.url.as_deref().is_none_or(str::is_empty) {
		return 1;
	}
	1 + embeds[1..]
		.iter()
		.take_while(|e| {
			eligible(e)
				&& e.url == first.url
				&& (e.title.is_none() || e.title == first.title)
				&& (e.description.is_none() || e.description == first.description)
				&& (e.author.is_none() || e.author == first.author)
				&& (e.provider.is_none() || e.provider == first.provider)
				&& (e.footer.is_none() || e.footer == first.footer)
				&& (e.timestamp.is_none() || e.timestamp == first.timestamp)
				&& (e.thumbnail.is_none() || e.thumbnail == first.thumbnail)
				&& (e.fields.is_empty() || e.fields == first.fields)
		})
		.count()
}

fn gallery_rect(count: usize, index: usize, width: f32) -> egui::Rect {
	let gap = 4.0_f32.min(width / 4.0);
	if count >= 5 && width >= 300.0 {
		let side = (width - 2.0 * gap) / 3.0;
		return egui::Rect::from_min_size(
			egui::pos2(
				(index % 3) as f32 * (side + gap),
				(index / 3) as f32 * (side + gap),
			),
			egui::Vec2::splat(side),
		);
	}
	let half = (width - gap) / 2.0;
	let (x, y, height) = if count == 3 {
		if index == 0 {
			(0.0, 0.0, width)
		} else {
			(half + gap, (index - 1) as f32 * (half + gap), half)
		}
	} else {
		(
			(index % 2) as f32 * (half + gap),
			(index / 2) as f32 * (half + gap),
			half,
		)
	};
	egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(half, height))
}

fn gallery(
	ui: &mut egui::Ui,
	embeds: &[Embed],
	images: &mut Avatars,
	message: model::Id,
	download: &mut DownloadUi,
	demo: bool,
) {
	let width = ui
		.available_width()
		.clamp(1.0, crate::avatars::media::MEDIA_MAX_WIDTH);
	let height = gallery_rect(embeds.len(), embeds.len() - 1, width).bottom();
	let (area, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
	for (index, embed) in embeds.iter().enumerate() {
		let rect = gallery_rect(embeds.len(), index, width).translate(area.min.to_vec2());
		ui.scope_builder(
			egui::UiBuilder::new()
				.id_salt(("gallery", index))
				.max_rect(rect),
			|ui| {
				let media = embed.image.as_ref().expect("gallery has images");
				let target = media
					.url
					.as_deref()
					.or(media.proxy_url.as_deref())
					.and_then(external_url);
				let image = images
					.show_media(ui, media, rect.size(), demo, Surface::Banner)
					.response;
				let response =
					ui.interact(image.rect, image.id.with("media"), egui::Sense::click());
				embed_context_menu(&response, media, download, demo);
				let star =
					gif_for_media(media, embed.url.as_deref(), embed.kind == "gifv").map(|gif| {
						let favorite = download.gif_favorites.contains(&gif.url);
						let star =
							favorite_star(ui, &response, favorite, images.gifs_playing(ui.ctx()));
						if star.clicked() {
							download.gif_favorite_request = Some(gif);
						}
						star
					});
				response.widget_info(|| {
					egui::WidgetInfo::labeled(
						if target.is_some() {
							egui::Role::Button
						} else {
							egui::Role::Image
						},
						ui.is_enabled(),
						format!(
							"{} {} {} {}",
							crate::i18n::translate("embeds-gallery-open-embed-image"),
							index + 1,
							crate::i18n::translate("embeds-gallery-of"),
							embeds.len()
						),
					)
				});
				if response.has_focus() {
					ui.painter().rect_stroke(
						rect,
						5,
						ui.visuals().selection.stroke,
						egui::StrokeKind::Inside,
					);
				}
				if target.is_some()
					&& !star
						.as_ref()
						.is_some_and(|star| star.hovered() || star.clicked())
					&& response
						.on_hover_text(crate::i18n::translate("embeds-gallery-open-image"))
						.clicked()
				{
					download.view_embed(message, media);
				}
			},
		);
	}
}

fn gif_for_embed(embed: &Embed, gifs: &client_core::gifs::Gifs) -> Option<Gif> {
	let media = [
		embed.video.as_ref().filter(|_| embed.kind == "gifv"),
		embed.image.as_ref(),
		embed.thumbnail.as_ref(),
	];
	let matches = |gif: &&Gif| {
		embed.url.as_deref() == Some(gif.url.as_str())
			|| media.iter().flatten().any(|image| {
				image
					.url
					.as_deref()
					.is_some_and(|url| url == gif.url || url == gif.preview)
			})
	};
	if let Some(gif) = gifs
		.favorites
		.iter()
		.find(matches)
		.or_else(|| gifs.view.as_ref()?.page.as_ref()?.gifs.iter().find(matches))
	{
		return Some(gif.clone());
	}
	media
		.iter()
		.flatten()
		.find_map(|media| gif_for_media(media, embed.url.as_deref(), embed.kind == "gifv"))
}

pub(crate) fn gif_for_media(
	media: &model::EmbedMedia,
	page: Option<&str>,
	animated: bool,
) -> Option<Gif> {
	let is_gif = |url: &str| {
		let path = url.split(['?', '#']).next().unwrap_or(url);
		let lower = path.to_ascii_lowercase();
		lower.ends_with(".gif")
	};
	let is_webp = |url: &str| {
		let path = url.split(['?', '#']).next().unwrap_or(url);
		let lower = path.to_ascii_lowercase();
		lower.ends_with(".webp")
	};
	let has_gif =
		media.url.as_deref().is_some_and(is_gif) || media.proxy_url.as_deref().is_some_and(is_gif);
	let has_webp = media.url.as_deref().is_some_and(is_webp)
		|| media.proxy_url.as_deref().is_some_and(is_webp);

	if !has_gif && !(animated && has_webp) && !animated {
		return None;
	}
	let preview = [media.url.as_deref(), media.proxy_url.as_deref()]
		.into_iter()
		.flatten()
		.find(|url| model::valid_gif_favorite_source(url))?;
	let url = page
		.filter(|url| model::valid_gif_favorite_url(url))
		.or(media
			.url
			.as_deref()
			.filter(|url| model::valid_gif_favorite_url(url)))
		.unwrap_or(preview);
	let mut hash = DefaultHasher::new();
	url.hash(&mut hash);
	let gif = Gif {
		id: format!("chat-{:016x}", hash.finish()),
		title: String::new(),
		url: url.to_owned(),
		preview: preview.to_owned(),
		width: media.width.clamp(1, 4096),
		height: media.height.clamp(1, 4096),
	};
	gif.valid().then_some(gif)
}

/// Discord shows a sizable landscape link preview thumbnail as the card's large image.
/// Bot (`rich`) embeds keep their explicit side thumbnail.
fn large_thumbnail(embed: &Embed) -> Option<&model::EmbedMedia> {
	embed.thumbnail.as_ref().filter(|thumbnail| {
		embed.image.is_none()
			&& matches!(embed.kind.as_str(), "article" | "link" | "video")
			&& thumbnail.width >= 300
			&& thumbnail.width as f32 >= thumbnail.height as f32 * 1.2
	})
}

/// Discord's closed-poll card: the winning answer and a jump back to the poll.
/// Returns whether "View Poll" was clicked; `can_open` reflects whether the poll is reachable.
pub fn poll_result(ui: &mut egui::Ui, message: &Message, can_open: bool) -> bool {
	let Some(embed) = message.embeds.iter().find(|e| e.kind == "poll_result") else {
		return false;
	};
	let number = |name| {
		embed
			.field(name)
			.and_then(|value| value.trim().parse::<u64>().ok())
	};
	let total = number("total_votes").unwrap_or(0);
	let winner = embed
		.field("victor_answer_text")
		.map(str::trim)
		.filter(|text| !text.is_empty());
	let (title, detail) = match winner {
		Some(text) => {
			let votes = number("victor_answer_votes").unwrap_or(0).min(total);
			let percent = (votes * 100 + total / 2).checked_div(total).unwrap_or(0);
			// Custom emoji images are not resolved here; Unicode emoji carry no id.
			let emoji = embed
				.field("victor_answer_emoji_name")
				.filter(|_| embed.field("victor_answer_emoji_id").is_none());
			(
				emoji.map_or_else(|| text.to_owned(), |emoji| format!("{emoji} {text}")),
				Some(format!(
					"{} • {percent}%",
					crate::i18n::translate("embeds-poll-result-winning-answer")
				)),
			)
		}
		None if total > 0 => (crate::i18n::translate("embeds-poll-result-tie"), None),
		None => (crate::i18n::translate("embeds-poll-result-no-votes"), None),
	};
	let colors = crate::design::palette(ui);
	let width = ui.available_width().min(440.0);
	let mut clicked = false;
	egui::Frame::new()
		.fill(crate::design::message_card_fill(ui, colors.raised))
		.stroke(crate::design::message_card_stroke(ui))
		.corner_radius(8)
		.inner_margin(egui::Margin::symmetric(16, 10))
		.show(ui, |ui| {
			ui.set_width((width - 34.0).max(1.0));
			ui.allocate_ui_with_layout(
				egui::vec2(ui.available_width(), crate::design::BUTTON_HEIGHT + 4.0),
				egui::Layout::right_to_left(egui::Align::Center),
				|ui| {
					clicked = ui
						.add_enabled_ui(can_open, |ui| {
							crate::design::button(
								ui,
								"embeds-poll-result-view-poll",
								crate::design::ButtonKind::Outline,
							)
						})
						.inner
						.clicked();
					ui.add_space(8.0);
					ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
						ui.spacing_mut().item_spacing.y = 2.0;
						if detail.is_none() {
							ui.add_space(10.0);
						}
						ui.horizontal(|ui| {
							ui.spacing_mut().item_spacing.x = 6.0;
							let badge = if winner.is_some() { 24.0 } else { 0.0 };
							ui.scope(|ui| {
								ui.set_max_width((ui.available_width() - badge).max(1.0));
								ui.add(
									egui::Label::new(
										crate::design::medium(ui, &title, 15.0)
											.color(colors.text_strong),
									)
									.truncate(),
								);
							});
							if winner.is_some() {
								let (rect, _) = ui.allocate_exact_size(
									egui::Vec2::splat(18.0),
									egui::Sense::hover(),
								);
								ui.painter()
									.circle_filled(rect.center(), 9.0, colors.positive);
								crate::icons::paint(
									ui.painter(),
									crate::icons::Icon::Check,
									rect.shrink(4.0),
									egui::Color32::WHITE,
								);
							}
						});
						if let Some(detail) = &detail {
							ui.label(RichText::new(detail).size(13.0).color(colors.muted));
						}
					});
				},
			);
		});
	ui.add_space(6.0);
	clicked && can_open
}

fn image_preview(
	ui: &mut egui::Ui,
	message: model::Id,
	image: &model::EmbedMedia,
	size: egui::Vec2,
	images: &mut Avatars,
	download: &mut DownloadUi,
	demo: bool,
) {
	let painted = images
		.show_media(ui, image, size, demo, Surface::Inline)
		.response;
	let response = ui.interact(painted.rect, painted.id.with("media"), egui::Sense::click());
	response.widget_info(|| {
		egui::WidgetInfo::labeled(
			egui::Role::Button,
			ui.is_enabled(),
			crate::i18n::translate("embeds-image-preview-image-actions"),
		)
	});
	embed_context_menu(&response, image, download, demo);
	if response.clicked() {
		download.view_embed(message, image);
	}
}

/// The clip of a poster-less video card, drawn where Discord draws its player.
fn clip_preview(ui: &mut egui::Ui, clip: &model::EmbedMedia, images: &mut Avatars, demo: bool) {
	images.show_media(
		ui,
		clip,
		egui::vec2(
			ui.available_width(),
			crate::avatars::media::MEDIA_MAX_HEIGHT,
		),
		demo,
		Surface::Inline,
	);
}

#[allow(clippy::too_many_arguments)]
pub fn show(
	ui: &mut egui::Ui,
	message: &Message,
	cache: &mut FormatCache,
	images: &mut Avatars,
	opening: &mut Option<String>,
	download: &mut DownloadUi,
	profile: &mut crate::profiles::ProfileSession,
	state: &client_core::State,
	card_surface: crate::design::MessageCardSurface,
	hide_media_links: bool,
) -> Option<Gif> {
	if message.embeds_suppressed {
		return None;
	}
	let source = crate::mentions::MentionSource {
		state,
		channel: message.channel,
	};
	let demo = state.demo;
	let mut favorite_action = None;
	let mut index = 0;
	while index < message.embeds.len() {
		let count = gallery_len(&message.embeds[index..]);
		let group = &message.embeds[index..index + count];
		let embed = &group[0];
		// Poll results render as a dedicated card next to their system row.
		if embed.kind == "poll_result" {
			index += count;
			continue;
		}
		let source_url = source_fallback(
			message,
			embed,
			cache,
			!hide_media_links || !standalone_media_links(message),
		);
		ui.push_id(("embed", index), |ui| {
			if count > 1 && inline_image(embed).is_some() {
				gallery(ui, group, images, message.id, download, demo);
				if let Some(url) = &source_url {
					link(
						ui,
						&crate::i18n::translate("embeds-link-open-link"),
						Some(url),
						opening,
						false,
					);
				}
				if group.iter().any(|e| e.limited) {
					ui.small(crate::i18n::translate("embeds-show-embed-display-limited"));
				}
				ui.add_space(6.0);
				return;
			}
			if let Some(image) = inline_image(embed) {
				let gif = gif_for_embed(embed, &state.gifs);
				let painted = images.show_gif_embed(
					ui,
					embed,
					gif.as_ref(),
					egui::vec2(
						ui.available_width()
							.min(crate::avatars::media::MEDIA_MAX_WIDTH),
						crate::avatars::media::MEDIA_MAX_HEIGHT,
					),
					demo,
				);
				let response =
					ui.interact(painted.rect, painted.id.with("media"), egui::Sense::click());
				response.widget_info(|| {
					egui::WidgetInfo::labeled(
						egui::Role::Button,
						ui.is_enabled(),
						crate::i18n::translate("embeds-show-open-image"),
					)
				});
				embed_context_menu(&response, image, download, demo);
				let star = gif.map(|gif| {
					let favorite = state.is_gif_favorite(&gif);
					let star =
						favorite_star(ui, &response, favorite, images.gifs_playing(ui.ctx()));
					if star.clicked() {
						favorite_action = Some(gif);
					}
					star
				});
				if !star
					.as_ref()
					.is_some_and(|star| star.hovered() || star.clicked())
					&& response
						.on_hover_text(crate::i18n::translate("embeds-show-open-image-2"))
						.clicked()
				{
					download.view_embed(message.id, image);
				}
				if let Some(url) = &source_url {
					link(
						ui,
						&crate::i18n::translate("embeds-link-open-link"),
						Some(url),
						opening,
						false,
					);
				}
				ui.add_space(6.0);
				return;
			}
			let colors = crate::design::palette(ui);
			let color = cache
				.embed_color(message.id, index, embed.color)
				.map_or(colors.muted, |c| {
					egui::Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
				});
			let width = ui.available_width().min(480.0);
			let frame = egui::Frame::new()
				.fill(card_surface.fill(ui, colors.raised))
				.stroke(crate::design::message_card_stroke(ui))
				.corner_radius(5)
				.inner_margin(12)
				.show(ui, |ui| {
					ui.set_width((width - 24.0).max(1.0));
					// Size independently of the remaining timeline viewport.
					ui.set_max_height(640.0);
					// Bound exceptional cards' geometry; normal cards grow to their content height.
					egui::ScrollArea::vertical()
						.id_salt("embed-content")
						.max_height(640.0)
						.auto_shrink([false, true])
						.show(ui, |ui| {
							let part = 1 + index as u16 * 64;
							let large = large_thumbnail(embed).filter(|_| count == 1);
							let thumbnail = embed
								.thumbnail
								.as_ref()
								.filter(|_| large.is_none() && ui.available_width() >= 300.0);
							let body_width = (ui.available_width()
								- if thumbnail.is_some() { 96.0 } else { 0.0 })
							.max(1.0);
							ui.horizontal_top(|ui| {
								ui.vertical(|ui| {
									ui.set_width(body_width);
									if let Some(provider) = &embed.provider {
										rich_text(
											ui,
											message,
											part + 32,
											&provider.name,
											false,
											Some(colors.muted),
											provider.url.as_deref(),
											cache,
											opening,
											profile,
											card_surface,
											images,
											demo,
											state,
										);
									}
									if let Some(author) = &embed.author {
										// Discord: a 24 px icon and the name in strong text,
										// a link only on hover.
										ui.horizontal_wrapped(|ui| {
											ui.spacing_mut().item_spacing.x = 8.0;
											if let Some(icon) = &author.icon {
												images.show_media(
													ui,
													icon,
													egui::vec2(24.0, 24.0),
													demo,
													Surface::Inline,
												);
											}
											rich_text(
												ui,
												message,
												part + 33,
												&author.name,
												true,
												Some(colors.text_strong),
												author.url.as_deref(),
												cache,
												opening,
												profile,
												card_surface,
												images,
												demo,
												state,
											);
										});
										ui.add_space(2.0);
									}
									if let Some(title) = &embed.title {
										rich_text(
											ui,
											message,
											part + 34,
											title,
											true,
											Some(ui.visuals().hyperlink_color),
											embed.url.as_deref(),
											cache,
											opening,
											profile,
											card_surface,
											images,
											demo,
											state,
										);
									}
									if let Some(description) = &embed.description {
										text(
											ui,
											message,
											(part, description),
											cache,
											opening,
											profile,
											card_surface,
											(images, demo, &state.guilds, &source),
										);
									}
								});
								if let Some(image) = thumbnail {
									image_preview(
										ui,
										message.id,
										image,
										egui::vec2(84.0, 84.0),
										images,
										download,
										demo,
									);
								}
							});
							let mut field = 0;
							while field < embed.fields.len() {
								let columns = if ui.available_width() >= 360.0 {
									3
								} else if ui.available_width() >= 240.0 {
									2
								} else {
									1
								};
								let count = if embed.fields[field].inline {
									embed.fields[field..]
										.iter()
										.take_while(|f| f.inline)
										.take(columns)
										.count()
								} else {
									1
								};
								ui.columns(count, |columns| {
									for (offset, column) in columns.iter_mut().enumerate() {
										let f = &embed.fields[field + offset];
										// Each field gets its own id namespace: unlike the cache
										// key above, egui's auto-assigned widget ids aren't
										// namespaced by field index, so adjacent fields can
										// otherwise collide and clash (visible as egui's debug
										// "used the same ID" warning in debug builds).
										column.push_id(field + offset, |column| {
											rich_text(
												column,
												message,
												part + 35 + (field + offset) as u16,
												&f.name,
												true,
												None,
												None,
												cache,
												opening,
												profile,
												card_surface,
												images,
												demo,
												state,
											);
											text(
												column,
												message,
												(part + 1 + (field + offset) as u16, &f.value),
												cache,
												opening,
												profile,
												card_surface,
												(images, demo, &state.guilds, &source),
											);
										});
									}
								});
								field += count;
							}
							if count > 1 {
								gallery(ui, group, images, message.id, download, demo);
							} else if let Some(image) = embed.image.as_ref().or(large) {
								image_preview(
									ui,
									message.id,
									image,
									egui::vec2(
										ui.available_width(),
										crate::avatars::media::MEDIA_MAX_HEIGHT,
									),
									images,
									download,
									demo,
								);
							} else if let Some(clip) = clip_media(embed) {
								clip_preview(ui, clip, images, demo);
							}
							if thumbnail.is_none()
								&& large.is_none() && let Some(image) = &embed.thumbnail
							{
								image_preview(
									ui,
									message.id,
									image,
									egui::vec2(84.0, 84.0),
									images,
									download,
									demo,
								);
							}
							if embed.video.is_some()
								|| matches!(embed.kind.as_str(), "video" | "gifv")
							{
								ui.small(crate::i18n::translate(
									"embeds-show-video-preview-playback-opens-in-your-browser",
								));
								link(
									ui,
									"Open video…",
									embed.url.as_deref().or_else(|| {
										embed.video.as_ref().and_then(|v| v.url.as_deref())
									}),
									opening,
									false,
								);
							}
							// One footer line, as on Discord: icon, text, then " • " and the
							// embed time in the chosen clock style.
							let when = embed.timestamp.as_deref().map(|raw| {
								time::OffsetDateTime::parse(
									raw,
									&time::format_description::well_known::Rfc3339,
								)
								.map_or_else(|_| raw.to_owned(), crate::local_time::calendar)
							});
							let footer_text = embed
								.footer
								.as_ref()
								.map(|footer| footer.text.as_str())
								.filter(|text| !text.is_empty());
							if footer_text.is_some() || when.is_some() {
								ui.add_space(4.0);
								ui.horizontal_wrapped(|ui| {
									ui.spacing_mut().item_spacing.x = 8.0;
									if let Some(icon) =
										embed.footer.as_ref().and_then(|f| f.icon.as_ref())
									{
										images.show_media(
											ui,
											icon,
											egui::vec2(20.0, 20.0),
											demo,
											Surface::Inline,
										);
									}
									if let Some(text) = footer_text {
										rich_text(
											ui,
											message,
											part + 60,
											text,
											false,
											Some(colors.muted),
											source_url.as_deref(),
											cache,
											opening,
											profile,
											card_surface,
											images,
											demo,
											state,
										);
									}
									if let Some(when) = when {
										ui.label(
											RichText::new(format!("• {when}"))
												.size(12.0)
												.color(colors.muted),
										);
									}
								});
							} else if let Some(url) = &source_url {
								link(
									ui,
									&crate::i18n::translate("embeds-link-open-link"),
									Some(url),
									opening,
									false,
								);
							}
							if group.iter().any(|e| e.limited) {
								ui.small(crate::i18n::translate(
									"embeds-show-embed-display-limited",
								));
							}
							if !matches!(
								embed.kind.as_str(),
								"rich" | "article" | "link" | "image" | "video" | "gifv"
							) {
								ui.small(crate::i18n::translate(
									"embeds-show-additional-embed-content-is-not-supported",
								));
							}
						});
				});
			ui.painter().line_segment(
				[
					frame.response.rect.left_top() + egui::vec2(1.5, 5.0),
					frame.response.rect.left_bottom() - egui::vec2(-1.5, 5.0),
				],
				egui::Stroke::new(3.0, color),
			);
			ui.add_space(6.0);
		});
		index += count;
	}
	favorite_action
}
pub fn estimated_height(embeds: &[Embed]) -> f32 {
	let mut height = 0.0;
	let mut index = 0;
	while index < embeds.len() {
		let count = gallery_len(&embeds[index..]);
		let e = &embeds[index];
		let image_height = if count > 1 {
			gallery_rect(count, count - 1, 456.0).bottom()
		} else if e.image.is_some() || large_thumbnail(e).is_some() || clip_media(e).is_some() {
			200.0
		} else {
			0.0
		};
		height += if e.kind == "poll_result" {
			72.0
		} else if inline_image(e).is_some() {
			let source = if e.kind == "image" && e.url.is_some() {
				20.0
			} else {
				0.0
			};
			(if count > 1 { image_height + 6.0 } else { 206.0 }) + source
		} else {
			let mut lines = 0.0;
			if e.provider
				.as_ref()
				.is_some_and(|provider| !provider.name.is_empty())
			{
				lines += 1.0;
			}
			if e.author.is_some() {
				lines += 1.0;
			}
			if e.title.is_some() {
				lines += 1.0;
			}
			if let Some(description) = e.description.as_deref().filter(|text| !text.is_empty()) {
				lines += description
					.lines()
					.map(|line| (line.chars().count() as f32 / 48.0).ceil().max(1.0))
					.sum::<f32>();
			}
			if e.footer
				.as_ref()
				.is_some_and(|footer| !footer.text.is_empty())
				|| e.timestamp.is_some()
				|| (e.url.is_some()
					&& e.title.as_deref().is_none_or(str::is_empty)
					&& e.video.is_none()
					&& !matches!(e.kind.as_str(), "video" | "gifv"))
			{
				lines += 1.0;
			}
			if lines == 0.0 {
				lines = 1.0;
			}
			let text = 24.0 + lines * 20.0 + 6.0;
			let thumb = if e.thumbnail.is_some() && large_thumbnail(e).is_none() {
				114.0
			} else {
				0.0
			};
			(text.max(thumb) + e.fields.len() as f32 * 44.0 + image_height).min(664.0)
		};
		index += count;
	}
	height
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A link that resolves to a hosted clip carries no poster; the card paints the clip.
	#[test]
	fn a_video_card_without_a_poster_shows_its_clip() {
		let mut embed = Embed {
			kind: "video".into(),
			url: Some("https://example.org/reels/synthetic/".into()),
			video: Some(model::EmbedMedia {
				url: Some("https://example.org/reels/synthetic/clip.mp4".into()),
				proxy_url: Some(
					"https://images-ext-1.discordapp.net/external/synthetic/https/example.org/reels/synthetic/clip.mp4"
						.into(),
				),
				width: 640,
				height: 1136,
				..Default::default()
			}),
			..Default::default()
		};
		let clip = clip_media(&embed).expect("the clip is the card's media");
		assert_eq!((clip.width, clip.height), (640, 1136));
		// The card asks the media worker for the clip through Discord's proxy.
		let mut message = test_support::message(1, model::Id(2));
		message.content = embed.url.clone().unwrap();
		message.embeds = vec![embed.clone()];
		let mut images = Avatars::default();
		egui::Context::default()
			.run_ui(egui::RawInput::default(), |ui| {
				show(
					ui,
					&message,
					&mut FormatCache::default(),
					&mut images,
					&mut None,
					&mut DownloadUi::default(),
					&mut crate::profiles::ProfileSession::default(),
					&client_core::State::default(),
					crate::design::MessageCardSurface::Conversation,
					false,
				);
			})
			.drop_without_applying_deltas();
		let proxied = clip.proxy_url.as_deref().unwrap();
		assert!(
			images
				.take_requests()
				.iter()
				.any(|request| request.starts_with("media:") && request.ends_with(proxied))
		);
		// A card with a picture keeps its poster or bot thumbnail instead.
		embed.thumbnail = Some(model::EmbedMedia {
			url: Some("https://example.org/reels/synthetic/poster.png".into()),
			..Default::default()
		});
		assert!(clip_media(&embed).is_none());
		embed.thumbnail = None;
		// Nothing to fetch is not a preview area.
		embed.video = Some(model::EmbedMedia::default());
		assert!(clip_media(&embed).is_none());
	}

	#[test]
	fn embed_shortcodes_keep_code_and_unknown_names_literal() {
		let state = client_core::State::default();
		let message = test_support::message(1, model::Id(2));
		let source = crate::mentions::MentionSource {
			state: &state,
			channel: model::Id(2),
		};
		assert_eq!(
			expand_shortcodes(
				":flag_br: :flag_us: :second_place: `:flag_us:` ```\n:flag_ca:\n``` :not_an_emoji:",
				&[],
				None,
				&message,
				&source
			),
			"🇧🇷 🇺🇸 🥈 `:flag_us:` ```\n:flag_ca:\n``` :not_an_emoji:"
		);
		assert_eq!(
			expand_shortcodes(
				"Here:**```yaml\n:second_place: :flag_us:",
				&[],
				None,
				&message,
				&source
			),
			"Here:**```yaml\n🥈 🇺🇸"
		);
	}

	#[test]
	fn server_shortcode_wins_only_when_it_belongs_to_the_message_guild() {
		let state = client_core::State::default();
		let message = test_support::message(1, model::Id(2));
		let source = crate::mentions::MentionSource {
			state: &state,
			channel: model::Id(2),
		};
		let guild = model::Guild {
			id: model::Id(1),
			name: "Synthetic".into(),
			emojis: Some(vec![model::CustomEmoji {
				id: model::Id(123),
				name: "alien".into(),
				animated: false,
				available: true,
				managed: false,
				roles: None,
			}]),
			default_message_notifications: None,
			stickers: None,
			icon: None,
		};
		assert_eq!(
			expand_shortcodes(
				"<@&834057797645631598> <:Vida:679480364489768970> <@644407668659978248> :alien:",
				&[guild],
				None,
				&message,
				&source
			),
			"<@&834057797645631598> <:Vida:679480364489768970> <@644407668659978248> 👽"
		);
	}

	#[test]
	fn message_bodies_resolve_shortcodes_only_from_their_server() {
		let mut state = client_core::State::default();
		state.channels.push(model::Channel {
			id: model::Id(2),
			guild: Some(model::Id(1)),
			name: "synthetic".into(),
			kind: 0,
			parent_id: None,
			position: 0,
			recipients: Vec::new(),
			member_list_id: None,
			tags: None,
			message_count: None,
			icon: None,
			last_message: None,
		});
		for (guild, name, emoji_id) in [(1, "warning", 123), (3, "alien", 456)] {
			state.guilds.push(model::Guild {
				id: model::Id(guild),
				name: "Synthetic".into(),
				emojis: Some(vec![model::CustomEmoji {
					id: model::Id(emoji_id),
					name: name.into(),
					animated: false,
					available: true,
					managed: false,
					roles: None,
				}]),
				default_message_notifications: None,
				stickers: None,
				icon: None,
			});
		}
		let mut message = test_support::message(1, model::Id(2));
		message.content = ":warning: :inbox_tray: :alien: `:warning:` :unknown:".into();
		message.author.kind = model::AccountKind::Bot;
		let expected = "<:warning:123> 📥 👽 `:warning:` :unknown:";
		assert_eq!(expanded_message_content(&message, &state), expected);
		message.author.kind = model::AccountKind::Human;
		message.author.webhook = true;
		assert_eq!(expanded_message_content(&message, &state), expected);
		message.author.webhook = false;
		message.author.kind = model::AccountKind::Human;
		assert_eq!(expanded_message_content(&message, &state), expected);
	}

	#[test]
	fn embed_snowflakes_next_to_custom_emoji_resolve_as_users_without_bracket_leaks() {
		let state = client_core::State::default();
		let mut message = test_support::message(1, model::Id(2));
		let mut user = message.author.clone();
		user.id = model::Id(644407668659978248);
		user.name = "Ranked player".into();
		message.mentions.push(user);
		let source = crate::mentions::MentionSource {
			state: &state,
			channel: model::Id(2),
		};
		assert_eq!(
			expand_shortcodes(
				"<:Vida:679480364489768970>644407668659978248>",
				&[],
				None,
				&message,
				&source
			),
			"<:Vida:679480364489768970><@644407668659978248>"
		);
	}

	#[test]
	fn embed_keeps_known_color_when_a_later_payload_omits_it() {
		let mut cache = FormatCache::default();
		let message = model::Id(42);
		assert_eq!(
			cache.embed_color(message, 0, Some(0xc76137)),
			Some(0xc76137)
		);
		assert_eq!(cache.embed_color(message, 0, None), Some(0xc76137));
		assert_eq!(cache.embed_color(message, 1, None), None);
		cache.retain(|_| false);
		assert_eq!(cache.embed_color(message, 0, None), None);
	}

	#[test]
	fn custom_emojis_render_in_every_text_section_of_an_embed() {
		let mut message = test_support::message(1, model::Id(2));
		message.embeds = vec![Embed {
			kind: "rich".into(),
			provider: Some(model::EmbedAuthor {
				name: "<:provider:9003>".into(),
				..Default::default()
			}),
			author: Some(model::EmbedAuthor {
				name: "<:author:9004>".into(),
				..Default::default()
			}),
			title: Some("<:title:9005>".into()),
			description: Some("<:description:9006>".into()),
			fields: vec![model::EmbedField {
				name: "<:field_name:9007>".into(),
				value: "<:field_value:9008>".into(),
				inline: false,
			}],
			footer: Some(model::EmbedFooter {
				text: "<:footer:9009>".into(),
				..Default::default()
			}),
			..Default::default()
		}];
		let ctx = egui::Context::default();
		let mut cache = FormatCache::default();
		let mut images = Avatars::default();
		let mut opening = None;
		let state = client_core::State::default();
		for _ in 0..2 {
			ctx.run_ui(
				egui::RawInput {
					screen_rect: Some(egui::Rect::from_min_size(
						egui::Pos2::ZERO,
						egui::vec2(640.0, 700.0),
					)),
					..Default::default()
				},
				|ui| {
					show(
						ui,
						&message,
						&mut cache,
						&mut images,
						&mut opening,
						&mut DownloadUi::default(),
						&mut crate::profiles::ProfileSession::default(),
						&state,
						crate::design::MessageCardSurface::Conversation,
						false,
					);
				},
			)
			.drop_without_applying_deltas();
		}
		let mut requests = images.take_requests();
		requests.sort();
		assert_eq!(
			requests,
			(9003..=9009)
				.map(|id| format!("emoji-{id}"))
				.collect::<Vec<_>>(),
			"provider, author, title, description, fields and footer all request emoji artwork"
		);
	}

	#[test]
	fn source_fallback_requires_a_safe_url_without_an_existing_source_action() {
		let mut message = test_support::message(1, model::Id(2));
		let mut embed = Embed {
			kind: "rich".into(),
			url: Some("https://example.org/post/1".into()),
			..Default::default()
		};
		let mut cache = FormatCache::default();
		assert_eq!(
			source_fallback(&message, &embed, &mut cache, true),
			embed.url
		);
		for content in [
			"https://example.org/post/1",
			"[Read the post](https://example.org/post/1)",
		] {
			message.content = content.into();
			assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		}
		for content in [
			"https://example.org/post/12",
			"`https://example.org/post/1`",
		] {
			message.content = content.into();
			assert_eq!(
				source_fallback(&message, &embed, &mut cache, true),
				embed.url
			);
		}
		embed.title = Some("A linked title".into());
		assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		embed.title = Some(String::new());
		embed.author = Some(model::EmbedAuthor {
			name: "Author".into(),
			url: embed.url.clone(),
			..Default::default()
		});
		assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		embed.provider = embed.author.take();
		assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		embed.provider = None;
		embed.kind = "video".into();
		assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		embed.kind = "rich".into();
		embed.video = Some(model::EmbedMedia::default());
		assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		embed.video = None;
		for url in [
			None,
			Some("javascript:alert(1)"),
			Some("https://user:password@example.org/post/1"),
		] {
			embed.url = url.map(str::to_owned);
			assert_eq!(source_fallback(&message, &embed, &mut cache, true), None);
		}
	}

	#[test]
	fn untitled_card_source_author_and_provider_dispatch_their_own_urls() {
		fn text_center(shape: &egui::Shape, label: &str) -> Option<egui::Pos2> {
			match shape {
				egui::Shape::Text(text) if text.galley.job.text == label => {
					Some(text.pos + text.galley.rect.center().to_vec2())
				}
				egui::Shape::Vec(shapes) => {
					shapes.iter().find_map(|shape| text_center(shape, label))
				}
				_ => None,
			}
		}
		for footer in [true, false] {
			let source_label = crate::i18n::translate("embeds-link-open-link");
			for (label, url) in [
				(
					if footer { "X" } else { &source_label },
					"https://example.org/post/1",
				),
				("Author", "https://example.org/author"),
				("Provider", "https://example.org/provider"),
			] {
				let mut message = test_support::message(1, model::Id(2));
				message.content = "A post without its URL in the message".into();
				message.embeds = vec![Embed {
					kind: "rich".into(),
					url: Some("https://example.org/post/1".into()),
					author: Some(model::EmbedAuthor {
						name: "Author".into(),
						url: Some("https://example.org/author".into()),
						..Default::default()
					}),
					provider: Some(model::EmbedAuthor {
						name: "Provider".into(),
						url: Some("https://example.org/provider".into()),
						..Default::default()
					}),
					footer: footer.then(|| model::EmbedFooter {
						text: "X".into(),
						..Default::default()
					}),
					..Default::default()
				}];
				let ctx = egui::Context::default();
				let mut opening = None;
				let mut cache = FormatCache::default();
				let mut images = Avatars::default();
				let mut frame = |events| {
					ctx.run_ui(
						egui::RawInput {
							screen_rect: Some(egui::Rect::from_min_size(
								egui::Pos2::ZERO,
								egui::vec2(640.0, 700.0),
							)),
							events,
							..Default::default()
						},
						|ui| {
							show(
								ui,
								&message,
								&mut cache,
								&mut images,
								&mut opening,
								&mut DownloadUi::default(),
								&mut crate::profiles::ProfileSession::default(),
								&client_core::State::default(),
								crate::design::MessageCardSurface::Conversation,
								false,
							);
						},
					)
				};
				frame(vec![]).drop_without_applying_deltas();
				let output = frame(vec![]);
				let pos = output
					.shapes
					.iter()
					.find_map(|shape| text_center(&shape.shape, label))
					.expect(label);
				output.drop_without_applying_deltas();
				for pressed in [true, false] {
					frame(vec![
						egui::Event::PointerMoved(pos),
						egui::Event::PointerButton {
							pos,
							button: egui::PointerButton::Primary,
							pressed,
							modifiers: egui::Modifiers::NONE,
						},
					])
					.drop_without_applying_deltas();
				}
				assert_eq!(opening.as_deref(), Some(url), "{label}, footer={footer}");
			}
		}
	}

	#[test]
	fn image_previews_dispatch_copy_and_save_without_opening_links() {
		for variant in 0..5 {
			for (copy, hide_media_links) in
				[(true, false), (false, false), (true, true), (false, true)]
			{
				let mut message = test_support::message(1, model::Id(2));
				let media = model::EmbedMedia {
					url: Some("https://cdn.discordapp.com/attachments/2/42/preview.png".into()),
					width: 160,
					height: 90,
					..Default::default()
				};
				message.embeds = vec![Embed {
					kind: match variant {
						0 | 4 => "image",
						1 => "gifv",
						_ => "rich",
					}
					.into(),
					url: Some("https://example.org/post".into()),
					image: (variant != 3).then(|| media.clone()),
					thumbnail: (variant == 3).then(|| media.clone()),
					..Default::default()
				}];
				if hide_media_links {
					message.content = message.embeds[0].url.clone().unwrap();
				}
				if variant == 4 {
					message.embeds.push(message.embeds[0].clone());
					message.embeds[0].image.as_mut().unwrap().url =
						Some("https://cdn.discordapp.com/attachments/2/43/other.png".into());
				}
				let ctx = egui::Context::default();
				let mut images = Avatars::default();
				let mut cache = FormatCache::default();
				let mut opening = None;
				let mut download = DownloadUi::default();
				let mut frame = |events| {
					ctx.run_ui(
						egui::RawInput {
							screen_rect: Some(egui::Rect::from_min_size(
								egui::Pos2::ZERO,
								egui::vec2(640.0, 700.0),
							)),
							events,
							..Default::default()
						},
						|ui| {
							let mut profile = crate::profiles::ProfileSession::default();
							assert!(
								show(
									ui,
									&message,
									&mut cache,
									&mut images,
									&mut opening,
									&mut download,
									&mut profile,
									&client_core::State::default(),
									crate::design::MessageCardSurface::Conversation,
									hide_media_links,
								)
								.is_none()
							);
						},
					)
				};
				frame(vec![]).drop_without_applying_deltas();
				let output = frame(vec![]);
				let pos = output
					.shapes
					.iter()
					.filter_map(|shape| match &shape.shape {
						egui::Shape::Rect(shape)
							if shape.corner_radius == egui::CornerRadius::same(5)
								&& shape.rect.width() > 40.0
								&& shape.rect.height() > 30.0 =>
						{
							Some(shape.rect.center())
						}
						_ => None,
					})
					.next_back()
					.expect("rendered image");
				assert_eq!(
					output.shapes.iter().any(|shape| matches!(&shape.shape,
						egui::Shape::Text(text) if text.galley.job.text == crate::i18n::translate("embeds-link-open-link")
					)),
					variant != 1 && (!hide_media_links || matches!(variant, 0 | 4)),
					"source action for media variant {variant}"
				);
				output.drop_without_applying_deltas();
				for pressed in [true, false] {
					frame(vec![
						egui::Event::PointerMoved(pos),
						egui::Event::PointerButton {
							pos,
							button: egui::PointerButton::Secondary,
							pressed,
							modifiers: egui::Modifiers::NONE,
						},
					])
					.drop_without_applying_deltas();
				}
				let output = frame(vec![]);
				let label = if copy {
					"Copy image"
				} else {
					"Save image as…"
				};
				let target = output
					.shapes
					.iter()
					.find_map(|shape| match &shape.shape {
						egui::Shape::Text(text) if text.galley.job.text == label => {
							Some(text.pos + text.galley.rect.center().to_vec2())
						}
						_ => None,
					})
					.unwrap_or_else(|| panic!("missing {label} for embed variant {variant}"));
				output.drop_without_applying_deltas();
				for pressed in [true, false] {
					frame(vec![
						egui::Event::PointerMoved(target),
						egui::Event::PointerButton {
							pos: target,
							button: egui::PointerButton::Primary,
							pressed,
							modifiers: egui::Modifiers::NONE,
						},
					])
					.drop_without_applying_deltas();
				}
				assert_eq!(download.embed_request, Some((media, copy)));
				assert!(
					download.request.is_none()
						&& download.copy_request.is_none()
						&& opening.is_none()
				);
			}
		}
	}

	fn gallery_embeds(count: usize) -> Vec<Embed> {
		(0..count)
			.map(|index| Embed {
				kind: "rich".into(),
				url: Some("https://example.com/gallery".into()),
				image: Some(model::EmbedMedia {
					url: Some(format!(
						"https://cdn.discordapp.com/attachments/1/2/{index}.png"
					)),
					width: 640,
					height: 360,
					..Default::default()
				}),
				..Default::default()
			})
			.collect()
	}

	#[test]
	fn gallery_groups_only_related_images_and_preserves_independent_content() {
		let mut embeds = gallery_embeds(3);
		embeds[0].title = Some("Gallery title".into());
		assert_eq!(gallery_len(&embeds), 3);
		assert!(estimated_height(&embeds) < 3.0 * 300.0);
		embeds[1].title = embeds[0].title.clone();
		assert_eq!(gallery_len(&embeds), 3);
		for variant in 0..7 {
			let mut separate = embeds.clone();
			match variant {
				0 => separate[1].url = Some("https://example.com/other".into()),
				1 => separate.iter_mut().for_each(|e| e.url = None),
				2 => separate
					.iter_mut()
					.for_each(|e| e.url = Some(String::new())),
				3 => separate[1].title = Some("Independent title".into()),
				4 => separate[1].image = None,
				5 => separate[1].video = Some(Default::default()),
				_ => separate[1].kind = "gifv".into(),
			}
			assert_eq!(gallery_len(&separate), 1, "variant {variant}");
		}
		assert_eq!(gallery_len(&[]), 0);
		assert_eq!(
			gallery_len(&gallery_embeds(model::MAX_EMBEDS)),
			model::MAX_EMBEDS
		);
	}

	#[test]
	fn hide_media_links_requires_matching_visible_media_without_caption_or_spoiler() {
		let mut message = test_support::message(1, model::Id(1));
		message.content = "https://klipy.com/gifs/waving-lizard".into();
		message.embeds = vec![Embed {
			kind: "gifv".into(),
			url: Some(message.content.clone()),
			thumbnail: Some(model::EmbedMedia::default()),
			..Default::default()
		}];
		assert!(standalone_media_links(&message));
		message.embeds_suppressed = true;
		assert!(!standalone_media_links(&message));
		message.embeds_suppressed = false;
		message.content.insert_str(0, "Hello! ");
		assert!(!standalone_media_links(&message));
		message.content = "https://example.com/other.gif".into();
		assert!(!standalone_media_links(&message));
		message.content = message.embeds[0].url.clone().unwrap();
		message.embeds[0].kind = "rich".into();
		assert!(!standalone_media_links(&message));
	}

	#[test]
	fn text_spoilers_do_not_hide_ordinary_media_but_keep_reply_previews_conservative() {
		let mut message = test_support::message(1, model::Id(1));
		message.embeds = vec![Embed::default()];
		message.attachments = vec![model::Attachment {
			duration_ms: None,
			waveform: Vec::new(),
			id: model::Id(2),
			filename: "photo.png".into(),
			description: None,
			content_type: Some("image/png".into()),
			size: 1,
			media: Default::default(),
			spoiler: false,
		}];
		message.content = "Ordinary text".into();
		assert!(!has_spoilers(&message));
		message.content = "Ordinary text ||hidden text||".into();
		assert!(!has_media_spoilers(&message));
		assert!(has_spoilers(&message));
		message.content.clear();
		for field in 0..10 {
			let mut guarded = message.clone();
			let embed = &mut guarded.embeds[0];
			match field {
				0 => guarded.attachments[0].spoiler = true,
				1 => guarded.attachments[0].filename = "SPOILER_photo.png".into(),
				2 => guarded.attachments[0].description = Some("||hidden||".into()),
				3 => embed.title = Some("||hidden||".into()),
				4 => embed.description = Some("||hidden||".into()),
				5 => embed.fields.push(model::EmbedField {
					name: "||hidden||".into(),
					..Default::default()
				}),
				6 => embed.fields.push(model::EmbedField {
					value: "||hidden||".into(),
					..Default::default()
				}),
				7 => {
					embed.author = Some(model::EmbedAuthor {
						name: "||hidden||".into(),
						..Default::default()
					})
				}
				8 => {
					embed.provider = Some(model::EmbedAuthor {
						name: "||hidden||".into(),
						..Default::default()
					})
				}
				_ => {
					embed.footer = Some(model::EmbedFooter {
						text: "||hidden||".into(),
						..Default::default()
					})
				}
			}
			assert!(has_media_spoilers(&guarded), "media safety field {field}");
			assert!(has_spoilers(&guarded), "reply safety field {field}");
		}
	}

	#[test]
	fn gif_for_media_ignores_static_webp_attachment() {
		let media = model::EmbedMedia {
			url: Some("https://cdn.discordapp.com/attachments/123/456/photo.webp".into()),
			width: 320,
			height: 200,
			..Default::default()
		};
		assert!(gif_for_media(&media, None, false).is_none());
	}

	#[test]
	fn gif_for_media_supports_animated_webp_provider_links() {
		let media = model::EmbedMedia {
			url: Some(
				"https://static.klipy.com/ii/71b2873e478b9d8d0482ea3ec777ba7f/f6/40/HtVp9vFM.webp"
					.into(),
			),
			width: 320,
			height: 200,
			..Default::default()
		};
		let gif = gif_for_media(&media, None, true).expect("animated webp recognized");
		assert_eq!(
			gif.url,
			"https://static.klipy.com/ii/71b2873e478b9d8d0482ea3ec777ba7f/f6/40/HtVp9vFM.webp"
		);
	}
}

/// A message GIF's overlay, Discord style. While GIFs are paused (the window is unfocused or
/// animation is off) a "GIF" label sits in the top-left corner; hovering the GIF drops a
/// favorite star into that corner instead, yellow under the pointer until it is a favorite.
pub(crate) fn favorite_star(
	ui: &mut egui::Ui,
	response: &egui::Response,
	favorite: bool,
	playing: bool,
) -> egui::Response {
	let palette = crate::design::palette(ui);
	let id = response.id.with("favorite");
	let star_rect = egui::Rect::from_min_size(
		response.rect.left_top() + egui::vec2(6.0, 6.0),
		egui::Vec2::splat(30.0),
	);
	let focused = ui.memory(|memory| memory.has_focus(id));
	let over = ui.rect_contains_pointer(response.rect.expand(2.0));
	// Drops in from above while the GIF is hovered; it only takes clicks once mostly shown.
	let shown = ui
		.ctx()
		.animate_bool_with_time(id.with("shown"), over || focused, 0.14);
	let star = ui.interact(
		star_rect,
		id,
		if shown > 0.5 {
			egui::Sense::click()
		} else {
			// Hidden, it still takes Tab focus, which reveals it for keyboard users.
			egui::Sense::focusable_noninteractive()
		},
	);
	if !playing && shown < 1.0 {
		let label = ui.painter().layout_no_wrap(
			"GIF".into(),
			egui::FontId::new(12.0, crate::design::semibold_family(ui.ctx())),
			egui::Color32::WHITE.gamma_multiply(1.0 - shown),
		);
		let badge = egui::Rect::from_min_size(
			response.rect.left_top() + egui::vec2(6.0, 6.0),
			label.size() + egui::vec2(12.0, 6.0),
		);
		ui.painter().rect_filled(
			badge,
			5,
			egui::Color32::from_black_alpha((170.0 * (1.0 - shown)) as u8),
		);
		ui.painter().galley(
			badge.min + egui::vec2(6.0, 3.0),
			label,
			egui::Color32::WHITE,
		);
	}
	if shown > 0.0 {
		let rect = star_rect.translate(egui::vec2(0.0, -8.0 * (1.0 - shown)));
		let hot = star.hovered() || star.has_focus();
		ui.painter().rect_filled(
			rect,
			6,
			egui::Color32::from_black_alpha((if hot { 215.0 } else { 175.0 } * shown) as u8),
		);
		let color = if favorite || hot {
			palette.warning
		} else {
			egui::Color32::WHITE
		};
		crate::icons::paint(
			ui.painter(),
			if favorite {
				crate::icons::Icon::StarFill
			} else {
				crate::icons::Icon::Star
			},
			rect.shrink(5.0),
			color.gamma_multiply(shown),
		);
		if star.has_focus() {
			ui.painter().rect_stroke(
				rect,
				6,
				ui.visuals().selection.stroke,
				egui::StrokeKind::Inside,
			);
		}
	}
	star.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::Role::CheckBox,
			ui.is_enabled(),
			favorite,
			crate::i18n::translate("embeds-show-favorite-gif"),
		)
	});
	if shown < 0.5 {
		return star;
	}
	star.on_hover_text(crate::i18n::translate_if_key(
		&(if favorite {
			crate::i18n::translate("embeds-show-remove-from-gif-favorites")
		} else {
			crate::i18n::translate("embeds-show-save-to-gif-favorites")
		}),
	))
}
