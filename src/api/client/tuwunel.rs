use std::time::SystemTime;

use axum::{
	Json,
	extract::{Query, State},
	http::HeaderMap,
	response::IntoResponse,
};
use ruma::api::client::error::ErrorKind::UnknownToken;
use ruma::events::room::message::RoomMessageEventContent;
use serde::Deserialize;
use tuwunel_core::{
	Err, Error::BadRequest, Result, err, is_less_than, utils::result::LogDebugErr,
};

/// # `GET /_tuwunel/server_version`
///
/// Tuwunel-specific API to get the server version, results akin to
/// `/_matrix/federation/v1/version`
pub(crate) async fn tuwunel_server_version() -> Result<impl IntoResponse> {
	Ok(Json(serde_json::json!({
		"name": tuwunel_core::version::name(),
		"version": tuwunel_core::version::version(),
	})))
}

/// # `GET /_tuwunel/local_user_count`
///
/// Tuwunel-specific API to return the amount of users registered on this
/// homeserver. Endpoint is disabled if federation is disabled for privacy. This
/// only includes active users (not deactivated, no guests, etc)
pub(crate) async fn tuwunel_local_user_count(
	State(services): State<crate::State>,
) -> Result<impl IntoResponse> {
	let user_count = services.users.list_local_users().count().await;

	Ok(Json(serde_json::json!({
		"count": user_count
	})))
}

/// Maximum UTF-8 length for [`AnnouncementsSendBody::body`] (avoids oversized timeline events).
const ANNOUNCEMENTS_SEND_BODY_MAX_BYTES: usize = 65_000;

/// # `POST /_matrix/client/v3/tuwunel/announcements/send` (and `r0`)
///
/// Sends a message as the system-notifications bot to every local user's announcements DM.
/// Same delivery path as `!admin server announcements-send`, but the full body is taken from
/// JSON so Markdown, newlines, and spacing are preserved.
///
/// Authentication: `Authorization: Bearer <access_token>` or query `?access_token=...` (Matrix
/// Client-Server convention). Caller must be a **server admin** (member of the admin room).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AnnouncementsSendQuery {
	access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnnouncementsSendBody {
	/// Full message text. Interpreted as CommonMark when `markdown` is true.
	body: String,
	/// When false, send plain text only (`m.text` without `formatted_body`).
	#[serde(default = "announcements_markdown_default")]
	markdown: bool,
}

fn announcements_markdown_default() -> bool { true }

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
	let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
	let rest = value
		.strip_prefix("Bearer ")
		.or_else(|| value.strip_prefix("bearer "))?;
	Some(rest.trim())
}

/// Shared implementation for `v3` and `r0` announcement routes.
pub(crate) async fn tuwunel_announcements_send(
	State(services): State<crate::State>,
	Query(query): Query<AnnouncementsSendQuery>,
	headers: HeaderMap,
	Json(payload): Json<AnnouncementsSendBody>,
) -> Result<impl IntoResponse> {
	let token = extract_bearer_token(&headers)
		.map(|s| s.to_owned())
		.or(query.access_token)
		.filter(|t| !t.is_empty())
		.ok_or_else(|| err!(Request(MissingToken("Missing access token."))))?;

	let (user_id, device_id, expires_at) = services
		.users
		.find_from_token(&token)
		.await
		.map_err(|_| BadRequest(UnknownToken { soft_logout: false }, "Unknown access token."))?;

	if expires_at.is_some_and(is_less_than!(SystemTime::now())) {
		services
			.users
			.remove_access_token(&user_id, &device_id)
			.await
			.log_debug_err()
			.ok();

		return Err(BadRequest(UnknownToken { soft_logout: true }, "Expired access token."));
	}

	if !services.admin.user_is_admin(&user_id).await {
		return Err!(Request(Forbidden(
			"Only server administrators may send system announcement messages.",
		)));
	}

	let markdown = payload.markdown;
	let body = payload.body;
	if body.trim().is_empty() {
		return Err!(Request(InvalidParam("JSON field `body` must be a non-empty string.")));
	}
	if body.len() > ANNOUNCEMENTS_SEND_BODY_MAX_BYTES {
		return Err!(Request(InvalidParam(
			"JSON field `body` exceeds maximum allowed size for this endpoint.",
		)));
	}

	let content = if markdown {
		RoomMessageEventContent::text_markdown(body)
	} else {
		RoomMessageEventContent::text_plain(body)
	};

	services.admin.send_announcements_message(content).await?;

	Ok(Json(serde_json::json!({
		"ok": true
	})))
}
