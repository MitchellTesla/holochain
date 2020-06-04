extern crate wee_alloc;

use holochain_wasmer_guest::*;
use holochain_zome_types::*;
use holochain_zome_types::migrate_agent::MigrateAgentCallbackResult;
use holochain_zome_types::globals::ZomeGlobals;

// Use `wee_alloc` as the global allocator.
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

holochain_wasmer_guest::holochain_externs!();

#[no_mangle]
pub extern "C" fn migrate_agent(_: RemotePtr) -> RemotePtr {
    let globals: ZomeGlobals = try_result!(host_call!(__globals, ()), "failed to get globals");
    ret!(GuestOutput::new(try_result!(MigrateAgentCallbackResult::Fail(globals.zome_name, "no migrate".into()).try_into(), "failed to serialize migrate agent return value")));
}