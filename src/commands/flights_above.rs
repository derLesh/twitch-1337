use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use eyre::Result;
use tokio::sync::Mutex;

use crate::aviation::AviationClient;
use super::{Command, CommandContext};

pub struct FlightsAboveCommand {
    aviation_client: Option<AviationClient>,
    cooldowns: Arc<Mutex<HashMap<String, std::time::Instant>>>,
}

impl FlightsAboveCommand {
    pub fn new(aviation_client: Option<AviationClient>) -> Self {
        Self {
            aviation_client,
            cooldowns: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Command for FlightsAboveCommand {
    fn name(&self) -> &str {
        "!up"
    }

    fn enabled(&self) -> bool {
        self.aviation_client.is_some()
    }

    async fn execute(&self, ctx: CommandContext<'_>) -> Result<()> {
        let client = self.aviation_client.as_ref().unwrap();
        let input: String = ctx.args.join(" ");
        crate::aviation::up_command(ctx.privmsg, ctx.client, client, &input, &self.cooldowns).await
    }
}
