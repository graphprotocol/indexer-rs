//! check if the query can be attestable generates attestation
//! executes query -> return subgraph response: (string, attestable (bool))
//! if attestable && allocation id:
//!     - look for signer
//!     - create attestation
//!     - return response with attestation
//! else:
//!     - return with no attestation
//!

// use crate::attestations::{signer::AttestationSigner, AttestableResponse, AttestationOutput};
// use alloy::primitives::Address;
// use anyhow::anyhow;
// use axum::{
//     extract::{Request, State},
//     middleware::Next,
//     response::Response,
// };
// use std::collections::HashMap;
// use tokio::sync::watch;
//
// use super::allocation::Allocation;
//
// pub struct MyState {
//     attestation_signers: watch::Receiver<HashMap<Address, AttestationSigner>>,
// }
//
// async fn attestation_middleware<T>(
//     State(state): State<MyState>,
//     request: Request<String>,
//     next: Next,
// ) -> Result<Response<T>, anyhow::Error> {
//     let Allocation(allocation_id) = request
//         .extensions()
//         .get::<Allocation>()
//         .ok_or(anyhow!("Could not find allocation"))?;
//
//     let (parts, req) = request.into_parts();
//
//     let signer = state
//         .attestation_signers
//         .borrow()
//         .get(allocation_id)
//         .cloned()
//         .ok_or_else(|| anyhow!("Error"))?;
//
//     let response: Box<dyn AttestableResponse<T>> = next.run(Request::new(req.into())).await.into();
//
//     let res = response.as_str();
//
//     let attestation = AttestationOutput::Attestation(
//         response
//             .is_attestable()
//             .then(|| signer.create_attestation(&req, res)),
//     );
//
//     let response = response.finalize(attestation);
//
//     Ok(Response::new(response))
// }
