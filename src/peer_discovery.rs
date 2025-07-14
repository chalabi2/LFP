// Since they are re-exported and used, but to suppress unused import warning if any, perhaps add #[allow(unused_imports)] but better to check if needed. Assuming previous uncomment fixed, no change if no error.
#[allow(unused_imports)]
pub use crate::discovery::{
    run_memory_only_discovery, spawn_discovery_service, spawn_memory_only_discovery,
    trigger_network_scan,
};
