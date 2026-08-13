pub mod cli;
pub mod cli_v2;
pub mod output;
pub mod ui;

use mko_core::{
    add::BatchAddResult,
    json_v1::{BatchAddData, BatchItemData},
};

pub fn batch_add_data(result: BatchAddResult) -> BatchAddData {
    BatchAddData {
        scan_complete: result.scan_complete,
        items: result
            .items
            .into_iter()
            .map(|item| BatchItemData {
                provider_locator: item.provider_locator,
                user_state: item.user_state,
                next_action: item.next_action,
                asset_id: item.asset_id,
                add_outcome: item.add_outcome,
                error: item.error,
            })
            .collect(),
        remaining: result.remaining,
    }
}

pub fn entry() {
    cli::entry();
}
