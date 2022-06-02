mod controller;
pub mod utils;
use controller::*;
use rocket::routes;

pub async fn init() -> Result<(), rocket::Error> {
    let _rocket = rocket::build()
        .mount("/", routes![index::index, index::about])
        .ignite()
        .await?
        .launch()
        .await?;

    Ok(())
}
