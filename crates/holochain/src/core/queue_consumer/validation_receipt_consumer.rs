//! The workflow and queue consumer for validation receipt

use super::*;

use crate::conductor::manager::ManagedTaskResult;
use crate::core::workflow::validation_receipt_workflow::validation_receipt_workflow;
use crate::core::workflow::validation_receipt_workflow::ValidationReceiptWorkspace;
use holochain_lmdb::env::EnvironmentWrite;

use tokio::task::JoinHandle;
use tracing::*;

/// Spawn the QueueConsumer for validation receipt workflow
#[instrument(skip(env, stop, cell_network))]
pub fn spawn_validation_receipt_consumer(
    env: EnvironmentWrite,
    mut stop: sync::broadcast::Receiver<()>,
    mut cell_network: HolochainP2pCell,
) -> (TriggerSender, JoinHandle<ManagedTaskResult>) {
    let (tx, mut rx) = TriggerSender::new();
    let mut trigger_self = tx.clone();
    let handle = tokio::spawn(async move {
        loop {
            // Wait for next job
            if let Job::Shutdown = next_job_or_exit(&mut rx, &mut stop).await {
                tracing::warn!(
                    "Cell is shutting down: stopping validation_receipt_workflow queue consumer."
                );
                break;
            }

            // Run the workflow
            let workspace = ValidationReceiptWorkspace::new(env.clone().into())
                .expect("Could not create ValidationReceiptWorkspace");
            if let WorkComplete::Incomplete =
                validation_receipt_workflow(workspace, env.clone().into(), &mut cell_network)
                    .await
                    .expect("Error running validation receipt workflow")
            {
                trigger_self.trigger()
            };
        }
        Ok(())
    });
    (tx, handle)
}
