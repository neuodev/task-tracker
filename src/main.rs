//! steps 
// ///!1. Setup the server 
// /// 2. Add CRUD 
// /// 3. Setup MongoDB 
// // 4. Custom Response 
// // 5. Error Handling 
//! 6. Testing
// // 7. Logging
////  8. Seed the database with many todos
//! 9. Add Pagination
use std::{str::FromStr};
use actix_web::middleware::Logger;
use env_logger::Env;
use log::{info};
use futures::stream::{TryStreamExt, StreamExt};
use actix_web::{ HttpServer, App, web, get, post, delete, put, Responder, HttpResponse, http::{header::ContentType, StatusCode}, body::{BoxBody}, ResponseError};
use rand::Rng;
use serde::{Serialize, Deserialize};
use mongodb::{ Client, options::{ClientOptions, UpdateModifications, FindOptions }, Collection, bson::{doc, oid::ObjectId, Bson}, Database};
use derive_more::{Display};
use serde_json::json;
use clap::Parser;


const ADDRESS: &str = "0.0.0.0:8080";

#[derive(Clone, Debug)]
struct AppState {
    db: Database,
    todo: Collection<Todo>
}

#[derive(Debug, Serialize, Display)]
enum ResErr {
    BadRequest(String),
    NotFound(String),
    #[display(fmt = "InvalidObjectIdError")]
    InvalidObjectId(String, String)
}


impl ResErr {
    fn err_msg(&self) -> String {
        match self {
            ResErr::InvalidObjectId(id, msg) => {
                let res = json!({
                    "id": id,
                    "message": msg
                });

                serde_json::to_string(&res).unwrap()
            },
            other => serde_json::to_string(&json!({ "message": other })).unwrap()
        }
    }
}

impl ResponseError for ResErr {
    fn error_response(&self) -> HttpResponse<BoxBody> {
        HttpResponse::build(self.status_code()).insert_header(ContentType::json()).body(self.err_msg())
    }

    fn status_code(&self) -> StatusCode {
        match self {
            ResErr::BadRequest(_) | ResErr::InvalidObjectId(_, _)=> StatusCode::BAD_REQUEST,
            ResErr::NotFound(_) => StatusCode::NOT_FOUND,
        }
    }
}

#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    env_logger::init_from_env(Env::default().default_filter_or("info"));
    
    let mut client_options = ClientOptions::parse("mongodb://root:changeme@localhost:27017").await.expect("Unable to connect to the DB");
    client_options.app_name = Some("todo".into());
    let client = Client::with_options(client_options).expect("Failed to connect to the client");
    let db = client.database("todo");
    info!("Connected to the database");

    let args = Args::parse();
    let db_clone = db.clone();
    if args.seed > 0 {
        tokio::spawn(async move {
            info!("Will create {} new todos", args.seed);
            let mut todos = vec![];
            for i in 0..=args.seed {
                todos.push(CreateTodo {
                    title: format!("Random {}", i),
                    is_done: rand::thread_rng().gen()
                })
            }
            let col = db_clone.collection::<CreateTodo>("todo");
            col.delete_many(doc! {}, None).await.unwrap(); // flush 
            info!("Database flushed");
            col.insert_many(todos, None).await.unwrap(); // seed
            info!("Database seeded")
        });    
    }
    info!("Server running on port 8080");
    HttpServer::new(move || {
        let state = web::Data::new(AppState{
            db: db.clone(),
            todo: db.collection("todo")
        });
        App::new()
        .wrap(Logger::default())
        .app_data(state.clone())
        .service(
            web::scope("/api/v1")
            .service(create_todo)
            .service(get_todo)
            .service(get_todos)
            .service(update_todo)
            .service(delete_todo)
        )
    }).bind(ADDRESS)?.run().await
}

#[derive(Debug, Serialize, Deserialize)]
struct Todo {
    _id: Option<ObjectId>,
    title: String,
    is_done: bool,
}

impl Responder for Todo {
    type Body = BoxBody;

    fn respond_to(self, _req: &actix_web::HttpRequest) -> HttpResponse<Self::Body> {
        let todo = serde_json::to_string(&self).unwrap();
        HttpResponse::Ok().content_type(ContentType::json()).body(todo)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateTodo {
    title: String,
    is_done: bool,
}

#[post("/todo")]
async fn create_todo(state: web::Data<AppState>, todo: web::Json<CreateTodo>) -> Result<IdResponse, impl ResponseError> {
    match state.db.collection("todo").insert_one(todo.into_inner(), None).await {
        Ok(res) => { 
            if let Bson::ObjectId(val) = res.inserted_id {
                return Ok(IdResponse { id: val.to_hex().to_string() })
            }

            return Err(ResErr::BadRequest(format!("Invalid response: {:#?}", res)))
        },
        Err(e) => Err(ResErr::BadRequest(format!("Failed to create todo: {}", e)))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TodosQuery {
    page_num: Option<u64>,
    page_size: Option<u64>
}

#[get("/todo")]
async fn get_todos(state: web::Data<AppState>, query: web::Query<TodosQuery>) -> Result<impl Responder, ResErr> {
    let page_size = query.page_size.unwrap_or_else(|| 10);
    let page_num = query.page_num.unwrap_or_else(|| 1);
    let query_options = FindOptions::builder().skip((page_num - 1) * page_size).limit(page_size as i64).build();
    let cursor = match state.todo.find(None, Some(query_options)).await {
        Ok(c) => c,
        Err(e) => return Err(ResErr::BadRequest(format!("Failed to get todos: {}", e)))
    };
    let todos: Vec<Todo> = match cursor.try_collect().await {
        Ok(todos) => todos,
        Err(e) => return Err(ResErr::BadRequest(format!("Failed to query todos: {e}")))
    };
    Ok(HttpResponse::Ok().content_type(ContentType::json()).body(serde_json::to_string(&todos).unwrap()))
}

#[get("/todo/{id}")]
async fn get_todo(state: web::Data<AppState>, id: web::Path<String>) -> Result<Todo, ResErr> {
    let id = id.into_inner();
    let _id = match ObjectId::parse_str(id.as_str()) {
        Ok(id) => id,
        Err(e) => return Err(ResErr::InvalidObjectId(id, e.to_string()))
    };
    match state.todo.find_one(Some(doc! { "_id": _id }), None).await {
        Ok(todo) => match todo {
            Some(todo) => Ok(todo),
            None => Err(ResErr::NotFound(format!("todo with id of {} is not found", id)))
        },
        Err(e) => Err(ResErr::BadRequest(format!("Unable to perform query: {}", e)))
    }
}


#[derive(Debug, Serialize, Deserialize)]
struct UpdateTodo {
    id: String,
    title: Option<String>,
    is_done: Option<bool>
}
#[put("/todo")]
async fn update_todo(state: web::Data<AppState> ,todo: web::Json<UpdateTodo>) -> Result<impl Responder, ResErr> {
    let todo = todo.into_inner();
    let oid = match ObjectId::from_str(todo.id.as_str()) {
        Ok(oid) => oid, 
        Err(e) => return Err(ResErr::InvalidObjectId(todo.id, e.to_string()))
    };

    // Check if todo exist or not 
    let found_todo = match state.todo.find_one(doc! { "_id": oid }, None).await {
        Ok(todo) => {
            match todo {
                Some(todo) => todo,
                None => return Err(ResErr::BadRequest(format!("todo not found")))
            }
        },
        Err(e) => return Err(ResErr::BadRequest(e.to_string()))
    };

    match state.todo.update_one(doc! { "_id": oid }, UpdateModifications::Document(doc! { "$set": { "title": todo.title.unwrap_or_else(|| found_todo.title), "is_done": todo.is_done.unwrap_or_else(|| found_todo.is_done) } }), None).await {
        Ok(_) => return Ok(IdResponse { id: todo.id }),
        Err(e) => return Err(ResErr::BadRequest(format!("Unable to update todo with id {}: {}", todo.id, e)))
    }
}

#[delete("/todo/{id}")]
async fn delete_todo(state: web::Data<AppState> ,id: web::Path<String>) -> Result<impl Responder, ResErr> {
    let id = id.into_inner();
    let oid = match ObjectId::from_str(id.as_str()) {
        Ok(id) => id,
        Err(e) => return Err(ResErr::InvalidObjectId(id.to_string(), e.to_string()))
    };
    // Check if todo exist or not 
    match state.todo.find_one(doc! { "_id": oid }, None).await {
        Ok(todo) => {
            if todo.is_none() {
                return Err(ResErr::BadRequest(format!("{} doesn't exist", id)))
            }
        },
        Err(e) => return Err(ResErr::BadRequest(e.to_string()))
    };
    
    match state.todo.delete_one(doc!{ "_id": oid }, None).await {
        Ok(_) => {
            Ok(IdResponse{ id })
        },
        Err(e) => Err(ResErr::BadRequest(e.to_string()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct IdResponse {
    id: String
}

impl Responder for IdResponse {
    type Body = BoxBody;
    fn respond_to(self, _req: &actix_web::HttpRequest) -> HttpResponse<Self::Body> {
        HttpResponse::Ok().content_type(ContentType::json()).body(serde_json::to_string(&self).unwrap())
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, value_parser, default_value_t = 0)]
    seed: u32
}