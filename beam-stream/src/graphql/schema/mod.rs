use async_graphql::*;

use admin::{AdminQuery, AdminSubscription};
use library::{LibraryMutation, LibraryQuery};
use media::{MediaMutation, MediaQuery};

pub mod admin;
pub mod library;
pub mod media;

#[derive(MergedObject, Default)]
pub struct QueryRoot(AdminQuery, LibraryQuery, MediaQuery);

#[derive(MergedObject, Default)]
pub struct MutationRoot(LibraryMutation, MediaMutation);

#[derive(MergedSubscription, Default)]
pub struct SubscriptionRoot(AdminSubscription);
