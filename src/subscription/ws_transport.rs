use crate::context::Data;
use crate::http::{GQLError, GQLRequest, GQLResponse};
use crate::{
    ObjectType, QueryResponse, Schema, SubscriptionStreams, SubscriptionTransport,
    SubscriptionType, Variables,
};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Serialize, Deserialize)]
struct OperationMessage {
    #[serde(rename = "type")]
    ty: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
}

/// WebSocket transport
#[derive(Default)]
pub struct WebSocketTransport {
    id_to_sid: HashMap<String, usize>,
    sid_to_id: HashMap<usize, String>,
    data: Arc<Data>,
    init_with_payload: Option<Box<dyn Fn(serde_json::Value) -> Data + Send + Sync>>,
}

impl WebSocketTransport {
    /// Creates a websocket transport and sets the function that converts the `payload` of the `connect_init` message to `Data`.
    pub fn new<F: Fn(serde_json::Value) -> Data + Send + Sync + 'static>(
        init_with_payload: F,
    ) -> Self {
        WebSocketTransport {
            init_with_payload: Some(Box::new(init_with_payload)),
            ..WebSocketTransport::default()
        }
    }
}

#[async_trait::async_trait]
impl SubscriptionTransport for WebSocketTransport {
    type Error = String;

    async fn handle_request<Query, Mutation, Subscription>(
        &mut self,
        schema: &Schema<Query, Mutation, Subscription>,
        streams: &mut SubscriptionStreams,
        data: Bytes,
    ) -> std::result::Result<Option<Bytes>, Self::Error>
    where
        Query: ObjectType + Sync + Send + 'static,
        Mutation: ObjectType + Sync + Send + 'static,
        Subscription: SubscriptionType + Sync + Send + 'static,
    {
        match serde_json::from_slice::<OperationMessage>(&data) {
            Ok(msg) => match msg.ty.as_str() {
                "connection_init" => {
                    if let Some(payload) = msg.payload {
                        if let Some(init_with_payload) = &self.init_with_payload {
                            self.data = Arc::new(init_with_payload(payload));
                        }
                    }
                    Ok(Some(
                        serde_json::to_vec(&OperationMessage {
                            ty: "connection_ack".to_string(),
                            id: None,
                            payload: None,
                        })
                        .unwrap()
                        .into(),
                    ))
                }
                "start" => {
                    if let (Some(id), Some(payload)) = (msg.id, msg.payload) {
                        if let Ok(request) = serde_json::from_value::<GQLRequest>(payload) {
                            let variables = request
                                .variables
                                .map(|value| Variables::parse_from_json(value).ok())
                                .flatten()
                                .unwrap_or_default();
                            match schema
                                .create_subscription_stream(
                                    &request.query,
                                    request.operation_name.as_deref(),
                                    variables,
                                    Some(self.data.clone()),
                                )
                                .await
                            {
                                Ok(stream) => {
                                    let stream_id = streams.add(stream);
                                    self.id_to_sid.insert(id.clone(), stream_id);
                                    self.sid_to_id.insert(stream_id, id);
                                    Ok(None)
                                }
                                Err(err) => Ok(Some(
                                    serde_json::to_vec(&OperationMessage {
                                        ty: "error".to_string(),
                                        id: Some(id),
                                        payload: Some(
                                            serde_json::to_value(GQLError(&err)).unwrap(),
                                        ),
                                    })
                                    .unwrap()
                                    .into(),
                                )),
                            }
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(None)
                    }
                }
                "stop" => {
                    if let Some(id) = msg.id {
                        if let Some(id) = self.id_to_sid.remove(&id) {
                            self.sid_to_id.remove(&id);
                            streams.remove(id);
                        }
                    }
                    Ok(None)
                }
                "connection_terminate" => Err("connection_terminate".to_string()),
                _ => Err("Unknown op".to_string()),
            },
            Err(err) => Err(err.to_string()),
        }
    }

    fn handle_response(&mut self, id: usize, value: serde_json::Value) -> Option<Bytes> {
        if let Some(id) = self.sid_to_id.get(&id) {
            Some(
                serde_json::to_vec(&OperationMessage {
                    ty: "data".to_string(),
                    id: Some(id.clone()),
                    payload: Some(
                        serde_json::to_value(GQLResponse(Ok(QueryResponse {
                            data: value,
                            extensions: None,
                            cache_control: Default::default(),
                        })))
                        .unwrap(),
                    ),
                })
                .unwrap()
                .into(),
            )
        } else {
            None
        }
    }
}
