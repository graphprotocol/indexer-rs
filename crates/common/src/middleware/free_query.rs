//! validates if query contains free auth token

use tower_http::{
    auth::require_authorization::Bearer, validate_request::ValidateRequestHeaderLayer,
};

pub fn new<ResBody>(bearer_token: String) -> ValidateRequestHeaderLayer<Bearer<ResBody>>
where
    ResBody: Default,
{
    ValidateRequestHeaderLayer::bearer(bearer_token.as_str())
}
