//! knowledge-scout core: normalization, vocabulary, cards, validation, candidate collection.

pub mod auth;
pub mod card;
pub mod collect;
pub mod index;
pub mod normalize;
pub mod queue;
pub mod testkit;
pub mod validate;
pub mod vocab;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Msg(String),
}

impl Error {
    pub fn msg(s: impl Into<String>) -> Self {
        Error::Msg(s.into())
    }
}
