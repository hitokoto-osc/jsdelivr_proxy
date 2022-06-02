#[derive(Clone, Debug)]
pub struct RequestId<T = String>(pub T);

// Allows a route to access the request id
#[rocket::async_trait]
impl<'r> FromRequest<'r> for RequestId {
    type Error = ();

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, ()> {
        match &*request.local_cache(|| RequestId::<Option<String>>(None)) {
            RequestId(Some(request_id)) => Outcome::Success(RequestId(request_id.to_owned())),
            RequestId(None) => Outcome::Failure((Status::InternalServerError, ())),
        }
    }
}