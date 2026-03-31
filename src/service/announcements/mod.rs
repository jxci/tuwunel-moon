//! Per-user system notices as 1:1 DMs with [`crate::globals::Service::announcements_bot_user`].

use std::collections::BTreeMap;

use futures::{FutureExt, StreamExt};
use ruma::{
	OwnedRoomId, OwnedUserId, RoomId, RoomVersionId, UserId,
	events::{
		GlobalAccountDataEventType,
		direct::{DirectEvent, OwnedDirectUserIdentifier},
		tag::TagName,
		room::{
			create::RoomCreateEventContent,
			guest_access::{GuestAccess, RoomGuestAccessEventContent},
			history_visibility::{HistoryVisibility, RoomHistoryVisibilityEventContent},
			join_rules::{JoinRule, RoomJoinRulesEventContent},
			member::{MembershipState, RoomMemberEventContent},
			message::RoomMessageEventContent,
			name::RoomNameEventContent,
			power_levels::RoomPowerLevelsEventContent,
			preview_url::RoomPreviewUrlsEventContent,
			topic::{RoomTopicEventContent, TopicContentBlock},
		},
	},
	int,
};
use tuwunel_core::{Result, error, info, pdu::PduBuilder, utils::ReadyExt, warn};

use crate::Services;

const WELCOME_SENT_KEY_PREFIX: &[u8] = b"announcements_welcome_v1\0";

fn welcome_sent_key(user_id: &UserId) -> Vec<u8> {
	let mut k = WELCOME_SENT_KEY_PREFIX.to_vec();
	k.extend_from_slice(user_id.as_bytes());
	k
}

/// Sends the configured Markdown welcome once per user (when `announcements_dm_welcome_message_path`
/// is set). Idempotent via global DB keys.
pub async fn send_welcome_once_if_configured(services: &Services, user_id: &UserId) -> Result {
	let Some(path) = services.server.config.announcements_dm_welcome_message_path.as_ref() else {
		return Ok(());
	};

	match services.db["global"].get(&welcome_sent_key(user_id)).await {
		| Ok(_) => return Ok(()),
		| Err(e) if e.is_not_found() => {},
		| Err(e) => return Err(e),
	}

	let text = match tokio::fs::read_to_string(path).await {
		| Ok(t) => t,
		| Err(e) => {
			warn!(
				%user_id,
				path = %path.display(),
				"Failed to read announcements welcome message file: {e}"
			);
			return Ok(());
		},
	};
	let text = text.trim();
	if text.is_empty() {
		return Ok(());
	}

	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	let Some(room_id) = find_announcements_dm_room(services, user_id).await? else {
		warn!(%user_id, "System notices DM room missing for welcome message");
		return Ok(());
	};

	match append_room_message(
		services,
		bot,
		&room_id,
		RoomMessageEventContent::text_markdown(text),
	)
	.await
	{
		| Ok(()) => {
			services.db["global"].insert(&welcome_sent_key(user_id), []);
			info!(%user_id, "Sent one-time system notices welcome message.");
		},
		| Err(e) => warn!(%user_id, %e, "Failed to send system notices welcome message"),
	}

	Ok(())
}

/// Creates the announcements bot account if missing.
pub async fn ensure_announcements_bot_user(services: &Services) -> Result {
	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	if !services.users.exists(bot).await {
		services.users.create(bot, None, None).await?;
	}
	Ok(())
}

/// True if `room_id` is a joined 2-member room containing only `user_id` and the announcements bot.
pub async fn room_is_announcements_dm_for_user(
	services: &Services,
	user_id: &UserId,
	room_id: &RoomId,
) -> bool {
	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	if user_id == bot {
		return false;
	}
	let Ok(count) = services.state_cache.room_joined_count(room_id).await else {
		return false;
	};
	if count != 2 {
		return false;
	}
	services.state_cache.is_joined(user_id, room_id).await
		&& services.state_cache.is_joined(bot, room_id).await
}

/// Resolves the announcements DM room for `user_id` if it exists and is valid.
pub async fn find_announcements_dm_room(
	services: &Services,
	user_id: &UserId,
) -> Result<Option<OwnedRoomId>> {
	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	let direct: DirectEvent = match services
		.account_data
		.get_global(bot, GlobalAccountDataEventType::Direct)
		.await
	{
		| Ok(d) => d,
		| Err(_) => return Ok(None),
	};

	for (peer, room_ids) in direct.content.0.iter() {
		if peer.as_user_id() != Some(user_id) {
			continue;
		}
		for room_id in room_ids {
			if room_is_announcements_dm_for_user(services, user_id, room_id).await {
				return Ok(Some(room_id.clone()));
			}
		}
	}

	Ok(None)
}

async fn merge_m_direct(
	services: &Services,
	owner: &UserId,
	peer: &UserId,
	room_id: &RoomId,
) -> Result {
	let mut event: DirectEvent = match services
		.account_data
		.get_global(owner, GlobalAccountDataEventType::Direct)
		.await
	{
		| Ok(e) => e,
		| Err(_) => DirectEvent {
			content: ruma::events::direct::DirectEventContent::default(),
		},
	};

	let peer_key = OwnedDirectUserIdentifier::from(peer.as_str());
	let rooms = event
		.content
		.0
		.entry(peer_key)
		.or_insert_with(Vec::new);
	if !rooms.iter().any(|r| r == room_id) {
		rooms.push(room_id.to_owned());
	}

	services
		.account_data
		.update(
			None,
			owner,
			GlobalAccountDataEventType::Direct.to_string().into(),
			&serde_json::to_value(&event)?,
		)
		.await
}

/// Ensures a DM room exists between `user_id` and the system-notices bot; idempotent.
pub async fn ensure_announcements_dm_for_user(services: &Services, user_id: &UserId) -> Result {
	if !services.server.config.announcements_dm_enabled {
		return Ok(());
	}

	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	if user_id == bot || !services.globals.user_is_local(user_id) {
		return Ok(());
	}

	ensure_announcements_bot_user(services).await?;

	if let Some(room_id) = find_announcements_dm_room(services, user_id).await? {
		if room_is_announcements_dm_for_user(services, user_id, &room_id).await {
			apply_announcements_dm_room_tags(services, &room_id, user_id).await;
		} else {
			// Stale m.direct entry; create a new room below.
			create_announcements_dm_room(services, user_id).await?;
		}
	} else {
		create_announcements_dm_room(services, user_id).await?;
	}

	send_welcome_once_if_configured(services, user_id).await?;
	Ok(())
}

async fn create_announcements_dm_room(services: &Services, user_id: &UserId) -> Result {
	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	let room_id = RoomId::new_v1(services.globals.server_name());
	let room_version = RoomVersionId::V11;

	let _short_id = services
		.short
		.get_or_create_shortroomid(&room_id)
		.await;

	let state_lock = services.state.mutex.lock(&room_id).await;

	let create_content = {
		use RoomVersionId::*;
		match room_version {
			| V1 | V2 | V3 | V4 | V5 | V6 | V7 | V8 | V9 | V10 =>
				RoomCreateEventContent::new_v1(bot.into()),
			| _ => RoomCreateEventContent::new_v11(),
		}
	};

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomCreateEventContent {
				federate: services.server.config.federate_announcements_dm,
				predecessor: None,
				room_version: room_version.clone(),
				..create_content
			}),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(
				String::from(bot),
				&RoomMemberEventContent::new(MembershipState::Join),
			),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	let mut users = BTreeMap::new();
	users.insert(bot.into(), int!(100));

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomPowerLevelsEventContent {
				ban: int!(50),
				events_default: int!(50),
				invite: int!(50),
				kick: int!(50),
				redact: int!(50),
				state_default: int!(50),
				users,
				users_default: int!(0),
				..Default::default()
			}),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomJoinRulesEventContent::new(JoinRule::Invite)),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(
				String::new(),
				&RoomHistoryVisibilityEventContent::new(HistoryVisibility::Shared),
			),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(
				String::new(),
				&RoomGuestAccessEventContent::new(GuestAccess::Forbidden),
			),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(
				String::new(),
				&RoomNameEventContent::new(
					services.server.config.announcements_dm_room_name.clone(),
				),
			),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomTopicEventContent {
				topic_block: TopicContentBlock::default(),
				topic: services.server.config.announcements_dm_room_topic.clone(),
			}),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::state(String::new(), &RoomPreviewUrlsEventContent { disabled: true }),
			bot,
			&room_id,
			&state_lock,
		)
		.boxed()
		.await?;

	drop(state_lock);

	services
		.membership
		.invite(bot, user_id, &room_id, None, true)
		.boxed()
		.await?;

	let state_lock = services.state.mutex.lock(&room_id).await;
	services
		.membership
		.join(
			user_id,
			&room_id,
			None,
			Some("Подключение к каналу системных уведомлений".to_owned()),
			&[],
			false,
			&state_lock,
		)
		.boxed()
		.await?;
	drop(state_lock);

	merge_m_direct(services, bot, user_id, &room_id).await?;
	merge_m_direct(services, user_id, bot, &room_id).await?;

	apply_announcements_dm_room_tags(services, &room_id, user_id).await;

	Ok(())
}

/// Sets `m.tag` keys from config for both the human and the announcements bot. Logs failures
/// but does not fail room creation.
async fn apply_announcements_dm_room_tags(
	services: &Services,
	room_id: &RoomId,
	human_user_id: &UserId,
) {
	let tags = &services.server.config.announcements_dm_room_tags;
	if tags.is_empty() {
		return;
	}
	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	for tag_str in tags {
		if tag_str.is_empty() {
			continue;
		}
		for owner in [human_user_id, bot] {
			let tag: TagName = tag_str.as_str().into();
			if let Err(e) = services
				.account_data
				.set_room_tag(owner, room_id, tag, None)
				.await
			{
				error!(?room_id, ?owner, ?tag_str, "Failed to set announcements DM room tag: {e}");
			}
		}
	}
}

/// Re-applies configured `m.tag` keys for every local user's valid announcements DM (human +
/// bot). Used by migration and admin command.
pub async fn backfill_announcements_dm_room_tags(services: &Services) -> Result {
	if !services.server.config.announcements_dm_enabled {
		return Ok(());
	}
	if services.server.config.announcements_dm_room_tags.is_empty() {
		return Ok(());
	}

	ensure_announcements_bot_user(services).await?;

	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	let users: Vec<OwnedUserId> = services
		.users
		.list_local_users()
		.ready_filter(|u| *u != bot)
		.map(|u| u.to_owned())
		.collect()
		.await;

	for user_id in users {
		if let Some(room_id) = find_announcements_dm_room(services, &user_id).await? {
			if room_is_announcements_dm_for_user(services, &user_id, &room_id).await {
				apply_announcements_dm_room_tags(services, &room_id, &user_id).await;
			}
		}
	}

	Ok(())
}

/// Creates missing DMs for every local user account (non-empty password), except the bot.
pub async fn backfill_announcements_dms_for_all_local_users(services: &Services) -> Result {
	if !services.server.config.announcements_dm_enabled {
		return Ok(());
	}

	ensure_announcements_bot_user(services).await?;

	let bot: &UserId = services.globals.announcements_bot_user.as_ref();
	let users: Vec<OwnedUserId> = services
		.users
		.list_local_users()
		.ready_filter(|u| *u != bot)
		.map(|u| u.to_owned())
		.collect()
		.await;

	for user_id in users {
		if let Err(e) = ensure_announcements_dm_for_user(services, &user_id).await {
			warn!(%user_id, %e, "Failed to ensure announcements DM during backfill");
		}
	}

	Ok(())
}

/// Appends a timeline message as `sender` (used for the system-notices bot).
pub async fn append_room_message(
	services: &Services,
	sender: &UserId,
	room_id: &RoomId,
	content: RoomMessageEventContent,
) -> Result {
	let state_lock = services.state.mutex.lock(room_id).await;
	services
		.timeline
		.build_and_append_pdu(
			PduBuilder::timeline(&content),
			sender,
			room_id,
			&state_lock,
		)
		.boxed()
		.await?;
	Ok(())
}
