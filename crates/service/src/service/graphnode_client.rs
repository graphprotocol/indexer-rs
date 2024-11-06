pub struct SubgraphServiceState {
    pub graph_node_client: reqwest::Client,
    pub graph_node_query_base_url: &'static Url,
}

struct SubgraphService {
    state: Arc<SubgraphServiceState>,
}

impl SubgraphService {
    fn new(state: Arc<SubgraphServiceState>) -> Self {
        Self { state }
    }
}

pub enum AttestationOutput {
    Attestation(Option<Attestation>),
    Attestable,
}

#[async_trait]
pub trait IndexerServiceImpl {
    type Error: std::error::Error;
    type Response: IndexerServiceResponse + Sized;
    type State: Send + Sync;

    async fn process_request<Request: DeserializeOwned + Send + Debug + Serialize>(
        &self,
        manifest_id: DeploymentId,
        request: Request,
    ) -> Result<(Request, Self::Response), Self::Error>;
}

#[async_trait]
impl IndexerServiceImpl for SubgraphService {
    type Error = SubgraphServiceError;
    type Response = SubgraphServiceResponse;
    type State = SubgraphServiceState;

    async fn process_request<Request: DeserializeOwned + Send + std::fmt::Debug + Serialize>(
        &self,
        deployment: DeploymentId,
        request: Request,
    ) -> Result<Self::Response, Self::Error> {
        let deployment_url = self
            .state
            .graph_node_query_base_url
            .join(&format!("subgraphs/id/{deployment}"))
            .map_err(|_| SubgraphServiceError::InvalidDeployment(deployment))?;

        let response = self
            .state
            .graph_node_client
            .post(deployment_url)
            .json(&request)
            .send()
            .await
            .map_err(SubgraphServiceError::QueryForwardingError)?;

        let attestable = response
            .headers()
            .get("graph-attestable")
            .map_or(false, |value| {
                value.to_str().map(|value| value == "true").unwrap_or(false)
            });

        let body = response
            .text()
            .await
            .map_err(SubgraphServiceError::QueryForwardingError)?;

        Ok(SubgraphServiceResponse::new(body, attestable))
    }
}
