//! Shared operations for CLI, TUI and other frontends. Desktop data access is limited to chats.
use crate::{
    auth,
    config::{self, BackendKind},
    desktop::{self, DesktopClient},
    error::AppError,
    graph::{GraphClient, Page},
};
use serde_json::Value;

pub enum TeamsBackend {
    Graph(GraphClient),
    Desktop(Box<DesktopClient>),
}

impl TeamsBackend {
    pub async fn connect(profile: Option<&str>) -> Result<Self, AppError> {
        let (name, selected) = config::load(profile)?;
        match selected.backend {
            BackendKind::Graph => Ok(Self::Graph(GraphClient::new(
                auth::access_token(&name, &selected).await?,
            ))),
            BackendKind::Desktop => Ok(Self::Desktop(Box::new(DesktopClient::with_account(
                selected.desktop_port,
                selected.desktop_account,
            )?))),
        }
    }

    fn graph(&self) -> Result<&GraphClient, AppError> {
        match self {
            Self::Graph(client) => Ok(client),
            Self::Desktop(_) => Err(desktop::messaging_unsupported()),
        }
    }

    pub async fn me(&self) -> Result<Value, AppError> {
        self.graph()?.me().await
    }
    pub async fn teams(&self, limit: u16, offset: usize) -> Result<Page, AppError> {
        self.graph()?.teams(limit, offset).await
    }
    pub async fn teams_available(&self) -> Result<(), AppError> {
        self.graph()?.teams_available().await
    }
    pub async fn channels(&self, team: &str, limit: u16, offset: usize) -> Result<Page, AppError> {
        self.graph()?.channels(team, limit, offset).await
    }
    pub async fn chats(&self, limit: u16, cursor: Option<&str>) -> Result<Page, AppError> {
        match self {
            Self::Graph(client) => client.chats(limit, cursor).await,
            Self::Desktop(client) => client.chats(limit, cursor).await,
        }
    }
    pub async fn messages(
        &self,
        chat: Option<&str>,
        team: Option<&str>,
        channel: Option<&str>,
        limit: u16,
        cursor: Option<&str>,
    ) -> Result<Page, AppError> {
        match (self, chat, team, channel) {
            (Self::Graph(client), ..) => client.messages(chat, team, channel, limit, cursor).await,
            (Self::Desktop(client), Some(chat), None, None) => {
                client.messages(chat, limit, cursor).await
            }
            (Self::Desktop(_), ..) => Err(desktop::messaging_unsupported()),
        }
    }
    pub async fn send(
        &self,
        chat: Option<&str>,
        team: Option<&str>,
        channel: Option<&str>,
        body: &str,
    ) -> Result<Value, AppError> {
        match (self, chat, team, channel) {
            (Self::Graph(client), ..) => client.send(chat, team, channel, body).await,
            (Self::Desktop(client), Some(chat), None, None) => client.send(chat, body).await,
            (Self::Desktop(_), ..) => Err(desktop::messaging_unsupported()),
        }
    }
}

impl TeamsBackend {
    pub async fn send_with_id(&self, chat: &str, body: &str, id: &str) -> Result<Value, AppError> {
        match self {
            Self::Desktop(client) => client.send_with_id(chat, body, id).await,
            Self::Graph(client) => client.send(Some(chat), None, None, body).await,
        }
    }
    pub async fn delivery(&self, chat: &str, id: &str) -> Result<Value, AppError> {
        match self {
            Self::Desktop(client) => client.delivery(chat, id).await,
            Self::Graph(_) => Err(AppError::Unsupported("delivery reconciliation by client message ID is available on desktop profiles only; check the conversation before resending".into())),
        }
    }
}
