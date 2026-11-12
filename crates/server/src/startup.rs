//! Startup: no authentication exchange, DSQL-shaped server parameters.
//!
//! Any user name and any password are accepted (constitution Principle I). The server does
//! not ask for a password at all, so clients that send one (an IAM token) and clients that
//! have none both connect.

use std::collections::HashMap;
use std::fmt::Debug;

use async_trait::async_trait;
use futures::Sink;
use pgwire::api::auth::{
    finish_authentication, protocol_negotiation, save_startup_parameters_to_metadata,
    ServerParameterProvider, StartupHandler,
};
use pgwire::api::{ClientInfo, METADATA_USER};
use pgwire::error::{PgWireError, PgWireResult};
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};

use micro_dsql_policy::server_params::STARTUP_PARAMETERS;

pub(crate) struct DsqlStartupHandler;

#[async_trait]
impl StartupHandler for DsqlStartupHandler {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        if let PgWireFrontendMessage::Startup(ref startup) = message {
            protocol_negotiation(client, startup).await?;
            save_startup_parameters_to_metadata(client, startup);
            finish_authentication(client, &DsqlParameters).await?;
        }
        Ok(())
    }
}

struct DsqlParameters;

impl ServerParameterProvider for DsqlParameters {
    fn server_parameters<C>(&self, client: &C) -> Option<HashMap<String, String>>
    where
        C: ClientInfo,
    {
        let mut params: HashMap<String, String> = STARTUP_PARAMETERS
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let metadata = client.metadata();
        params.insert(
            "application_name".to_owned(),
            metadata
                .get("application_name")
                .cloned()
                .unwrap_or_default(),
        );
        if let Some(user) = metadata.get(METADATA_USER) {
            params.insert("session_authorization".to_owned(), user.clone());
        }
        Some(params)
    }
}
