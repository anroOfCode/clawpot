mod store;
mod types;

pub use store::{EventStore, PersistMode};
#[cfg_attr(not(test), allow(unused_imports))]
pub use types::{Event, EventFilters, SessionInfo};

/// Emit a structured event with typed data.
///
/// ```ignore
/// // With vm_id:
/// clawpot_event!(events, "vm.create.ip_allocated", "vm", vm_id = vm_id, {"ip_address": ip});
/// // Without vm_id:
/// clawpot_event!(events, "server.started", "server", {"version": ver});
/// ```
#[macro_export]
macro_rules! clawpot_event {
    ($store:expr, $event_type:expr, $category:expr, vm_id = $vm_id:expr, $data:tt) => {{
        let _ = $store.emit(
            $event_type,
            $category,
            Some(&$vm_id.to_string()),
            None,
            &serde_json::json!($data),
        );
    }};
    ($store:expr, $event_type:expr, $category:expr, $data:tt) => {{
        let _ = $store.emit(
            $event_type,
            $category,
            None,
            None,
            &serde_json::json!($data),
        );
    }};
}

/// Emit a simple log message event.
///
/// ```ignore
/// clawpot_log!(events, "server", "Network bridge ready");
/// clawpot_log!(events, "vm", vm_id = vm_id, "VM started successfully");
/// ```
#[macro_export]
macro_rules! clawpot_log {
    ($store:expr, $category:expr, vm_id = $vm_id:expr, $($arg:tt)*) => {{
        let _ = $store.log($category, Some(&$vm_id.to_string()), &format!($($arg)*));
    }};
    ($store:expr, $category:expr, $($arg:tt)*) => {{
        let _ = $store.log($category, None, &format!($($arg)*));
    }};
}
