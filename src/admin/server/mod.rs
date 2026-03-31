mod commands;

use std::path::PathBuf;

use clap::Subcommand;
use tuwunel_core::Result;

use crate::admin_command_dispatch;

#[admin_command_dispatch]
#[derive(Debug, Subcommand)]
pub(super) enum ServerCommand {
	/// - Time elapsed since startup
	Uptime,

	/// - Show configuration values
	ShowConfig,

	/// - Reload configuration values
	ReloadConfig {
		path: Option<PathBuf>,
	},

	/// - List the features built into the server
	ListFeatures {
		#[arg(short, long)]
		available: bool,

		#[arg(short, long)]
		enabled: bool,

		#[arg(short, long)]
		comma: bool,
	},

	/// - Print database memory usage statistics
	MemoryUsage,

	/// - Clears all of Tuwunel's caches
	ClearCaches,

	/// - Performs an online backup of the database (only available for RocksDB
	///   at the moment)
	BackupDatabase,

	/// - List database backups
	ListBackups,

	/// - Send a message to the admin room.
	AdminNotice {
		message: Vec<String>,
	},

	/// - Post a markdown message to every local user's system-notices DM (from
	///   the configured localpart, e.g. `@system_notices`, when `announcements_dm_enabled` is true).
	AnnouncementsSend {
		message: Vec<String>,
	},

	/// - Re-apply configured `m.tag` keys on every local user's system-notices DM (human +
	///   bot), e.g. after changing `announcements_dm_room_tags`.
	AnnouncementsRetag,

	/// - Hot-reload the server
	#[clap(alias = "reload")]
	ReloadMods,

	#[cfg(unix)]
	/// - Restart the server
	Restart {
		#[arg(short, long)]
		force: bool,
	},

	/// - Shutdown the server
	Shutdown,
}
