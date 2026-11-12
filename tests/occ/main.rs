//! OCC scenarios (M4): two or more sessions against one in-process micro-dsql server, one
//! scenario per cell of the conflict matrix of research/01 section 2 (see README.md).

#[allow(dead_code)]
#[path = "../../crates/server/tests/smoke/common.rs"]
mod common;

mod autocommit;
mod differences;
mod locking;
mod matrix;
mod snapshot;

use tokio_postgres::Client;

use common::{DbErr, TestServer};

pub const OC000: &str = "change conflicts with another transaction (OC000)";
pub const OC001: &str = "schema has been updated by another transaction (OC001)";

/// One client connection with step helpers (simple protocol unless noted).
#[allow(dead_code)]
pub struct Session {
    pub client: Client,
}

#[allow(dead_code)]
impl Session {
    pub async fn new(server: &TestServer) -> Self {
        Self {
            client: server.connect().await,
        }
    }

    /// Runs the statement(s); panics on error.
    pub async fn ok(&self, sql: &str) {
        if let Err(e) = self.client.simple_query(sql).await {
            panic!("{sql}: {e:?}");
        }
    }

    /// The error of a statement that must fail.
    pub async fn err(&self, sql: &str) -> DbErr {
        match self.client.simple_query(sql).await {
            Ok(_) => panic!("{sql}: expected an error"),
            Err(e) => DbErr::of(&e),
        }
    }

    /// The single text value of a one-row, one-column query.
    pub async fn value(&self, sql: &str) -> Option<String> {
        common::value(&self.client, sql).await
    }

    /// The rows of a query as text.
    pub async fn rows(&self, sql: &str) -> Vec<Vec<Option<String>>> {
        common::rows(&self.client, sql).await
    }

    /// The result of a statement without panicking.
    pub async fn try_ok(&self, sql: &str) -> Result<(), DbErr> {
        self.client
            .simple_query(sql)
            .await
            .map(|_| ())
            .map_err(|e| DbErr::of(&e))
    }

    /// The rows-affected count of a statement's command tag.
    pub async fn affected(&self, sql: &str) -> u64 {
        for message in self
            .client
            .simple_query(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"))
        {
            if let tokio_postgres::SimpleQueryMessage::CommandComplete(n) = message {
                return n;
            }
        }
        panic!("{sql}: no command tag");
    }
}

pub fn oc000() -> DbErr {
    DbErr::new("40001", OC000)
}

pub fn oc001() -> DbErr {
    DbErr::new("40001", OC001)
}
