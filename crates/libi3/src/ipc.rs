//! C: include/i3/ipc.h — constants of the IPC interface to i3.

/// C: include/i3/ipc.h:15 typedef struct i3_ipc_header (packed).
/// magic (6 bytes) + size + type, all in native endianness.
#[repr(C, packed)]
pub struct i3_ipc_header_t {
    /// C: char magic[6] — 6 = strlen(I3_IPC_MAGIC).
    pub magic: [u8; 6],
    /// C: uint32_t size.
    pub size: u32,
    /// C: uint32_t type ("type" is a Rust keyword; kept as r#type).
    pub r#type: u32,
}

/// C: include/i3/ipc.h:31 — Never change this, only on major IPC breakage.
pub const I3_IPC_MAGIC: &str = "i3-ipc";

// Messages from clients to i3 (C: include/i3/ipc.h:33-75)

/// C: include/i3/ipc.h:36 — Deprecated: use I3_IPC_MESSAGE_TYPE_RUN_COMMAND.
pub const I3_IPC_MESSAGE_TYPE_COMMAND: u32 = 0;
/// C: include/i3/ipc.h:39 — The payload of the message will be interpreted as a command.
pub const I3_IPC_MESSAGE_TYPE_RUN_COMMAND: u32 = 0;
/// C: include/i3/ipc.h:42 — Requests the current workspaces from i3.
pub const I3_IPC_MESSAGE_TYPE_GET_WORKSPACES: u32 = 1;
/// C: include/i3/ipc.h:45 — Subscribe to the specified events.
pub const I3_IPC_MESSAGE_TYPE_SUBSCRIBE: u32 = 2;
/// C: include/i3/ipc.h:48 — Requests the current outputs from i3.
pub const I3_IPC_MESSAGE_TYPE_GET_OUTPUTS: u32 = 3;
/// C: include/i3/ipc.h:51 — Requests the tree layout from i3.
pub const I3_IPC_MESSAGE_TYPE_GET_TREE: u32 = 4;
/// C: include/i3/ipc.h:54 — Request the current defined marks from i3.
pub const I3_IPC_MESSAGE_TYPE_GET_MARKS: u32 = 5;
/// C: include/i3/ipc.h:57 — Request the configuration for a specific 'bar'.
pub const I3_IPC_MESSAGE_TYPE_GET_BAR_CONFIG: u32 = 6;
/// C: include/i3/ipc.h:60 — Request the i3 version.
pub const I3_IPC_MESSAGE_TYPE_GET_VERSION: u32 = 7;
/// C: include/i3/ipc.h:63 — Request a list of configured binding modes.
pub const I3_IPC_MESSAGE_TYPE_GET_BINDING_MODES: u32 = 8;
/// C: include/i3/ipc.h:66 — Request the raw last loaded i3 config.
pub const I3_IPC_MESSAGE_TYPE_GET_CONFIG: u32 = 9;
/// C: include/i3/ipc.h:69 — Send a tick event to all subscribers.
pub const I3_IPC_MESSAGE_TYPE_SEND_TICK: u32 = 10;
/// C: include/i3/ipc.h:72 — Trigger an i3 sync protocol message via IPC.
pub const I3_IPC_MESSAGE_TYPE_SYNC: u32 = 11;
/// C: include/i3/ipc.h:75 — Request the current binding state.
pub const I3_IPC_MESSAGE_TYPE_GET_BINDING_STATE: u32 = 12;

// Messages from i3 to clients (C: include/i3/ipc.h:80-93)

/// C: include/i3/ipc.h:81
pub const I3_IPC_REPLY_TYPE_COMMAND: u32 = 0;
/// C: include/i3/ipc.h:82
pub const I3_IPC_REPLY_TYPE_WORKSPACES: u32 = 1;
/// C: include/i3/ipc.h:83
pub const I3_IPC_REPLY_TYPE_SUBSCRIBE: u32 = 2;
/// C: include/i3/ipc.h:84
pub const I3_IPC_REPLY_TYPE_OUTPUTS: u32 = 3;
/// C: include/i3/ipc.h:85
pub const I3_IPC_REPLY_TYPE_TREE: u32 = 4;
/// C: include/i3/ipc.h:86
pub const I3_IPC_REPLY_TYPE_MARKS: u32 = 5;
/// C: include/i3/ipc.h:87
pub const I3_IPC_REPLY_TYPE_BAR_CONFIG: u32 = 6;
/// C: include/i3/ipc.h:88
pub const I3_IPC_REPLY_TYPE_VERSION: u32 = 7;
/// C: include/i3/ipc.h:89
pub const I3_IPC_REPLY_TYPE_BINDING_MODES: u32 = 8;
/// C: include/i3/ipc.h:90
pub const I3_IPC_REPLY_TYPE_CONFIG: u32 = 9;
/// C: include/i3/ipc.h:91
pub const I3_IPC_REPLY_TYPE_TICK: u32 = 10;
/// C: include/i3/ipc.h:92
pub const I3_IPC_REPLY_TYPE_SYNC: u32 = 11;
/// C: include/i3/ipc.h:93
pub const I3_IPC_REPLY_TYPE_GET_BINDING_STATE: u32 = 12;

// Events from i3 to clients — events have the first bit set high
// (C: include/i3/ipc.h:98-120)

/// C: include/i3/ipc.h:98
pub const I3_IPC_EVENT_MASK: u32 = 1 << 31;
/// C: include/i3/ipc.h:101 — The workspace event will be triggered upon changes in the workspace list.
pub const I3_IPC_EVENT_WORKSPACE: u32 = I3_IPC_EVENT_MASK | 0;
/// C: include/i3/ipc.h:104 — The output event will be triggered upon changes in the output list.
pub const I3_IPC_EVENT_OUTPUT: u32 = I3_IPC_EVENT_MASK | 1;
/// C: include/i3/ipc.h:107 — The output event will be triggered upon mode changes.
pub const I3_IPC_EVENT_MODE: u32 = I3_IPC_EVENT_MASK | 2;
/// C: include/i3/ipc.h:110 — The window event will be triggered upon window changes.
pub const I3_IPC_EVENT_WINDOW: u32 = I3_IPC_EVENT_MASK | 3;
/// C: include/i3/ipc.h:113 — Bar config update will be triggered to update the bar config.
pub const I3_IPC_EVENT_BARCONFIG_UPDATE: u32 = I3_IPC_EVENT_MASK | 4;
/// C: include/i3/ipc.h:116 — The binding event will be triggered when bindings run.
pub const I3_IPC_EVENT_BINDING: u32 = I3_IPC_EVENT_MASK | 5;
/// C: include/i3/ipc.h:119 — The shutdown event will be triggered when the ipc shuts down.
pub const I3_IPC_EVENT_SHUTDOWN: u32 = I3_IPC_EVENT_MASK | 6;
/// C: include/i3/ipc.h:122 — The tick event will be sent upon a tick IPC message.
pub const I3_IPC_EVENT_TICK: u32 = I3_IPC_EVENT_MASK | 7;
