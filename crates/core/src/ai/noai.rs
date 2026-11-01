use std::time::Duration;

use super::{
    Availability, BoxFuture, CancellationToken, Capability, Declaration, Prompt, Provider,
    ProviderId,
};
use crate::error::{Error, Result};

/// The path that always exists. It invents nothing: every call asks for manual work.
pub struct NoAi;

impl Provider for NoAi {
    fn declaration(&self) -> Declaration {
        Declaration {
            id: ProviderId::NoAi,
            capabilities: vec![Capability::Offline],
            local: true,
            context_limit: 0,
            timeout: Duration::ZERO,
            sign_in: "none".into(),
        }
    }

    fn available(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async { Availability::Available })
    }

    fn complete<'a>(
        &'a self,
        _prompt: &'a Prompt,
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async { Err(Error::ManualRequired("no_ai".into())) })
    }

    fn embed<'a>(
        &'a self,
        _model: &'a str,
        _inputs: &'a [String],
        _cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<Vec<f32>>>> {
        Box::pin(async { Err(Error::ManualRequired("no_embedding_model".into())) })
    }
}
