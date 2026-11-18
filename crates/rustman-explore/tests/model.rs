//! Task 1.1: OpenAPI 3.0 and 3.1 documents lower to the same `ApiModel`.

mod common;

use common::load;
use rustman_explore::openapi::model::{ParameterLocation, SecurityScheme};
use rustman_explore::openapi::{Format, SpecVersion};
use serde_json::json;

#[tokio::test]
async fn petstore_3_0_and_3_1_lower_to_the_same_ir() {
    let v30 = load("petstore-3.0.yaml").await;
    let v31 = load("petstore-3.1.json").await;
    assert_eq!((v30.version, v30.format), (SpecVersion::V3_0, Format::Yaml));
    assert_eq!((v31.version, v31.format), (SpecVersion::V3_1, Format::Json));
    assert_eq!(v30.model, v31.model);
    assert!(v30.warnings.is_empty(), "{:?}", v30.warnings);
}

#[tokio::test]
async fn nullable_is_rewritten_to_a_null_type() {
    let model = load("petstore-3.0.yaml").await.model;
    let schemas = &model.components["schemas"];
    assert_eq!(
        schemas["NewPet"]["properties"]["tag"],
        json!({"type": ["string", "null"]})
    );
    assert_eq!(
        schemas["NewPet"]["properties"]["status"]["enum"],
        json!(["available", "sold", null])
    );
    assert_eq!(
        schemas["Owner"],
        json!({"anyOf": [{"$ref": "#/components/schemas/Pet"}, {"type": "null"}]})
    );
    let pet_id = &model.operations[2].parameters[0];
    assert_eq!(
        pet_id.schema.as_ref().unwrap()["exclusiveMinimum"],
        json!(0)
    );
    assert!(pet_id.schema.as_ref().unwrap().get("minimum").is_none());
}

#[tokio::test]
async fn operations_parameters_and_bodies() {
    let model = load("petstore-3.0.yaml").await.model;
    assert_eq!(model.title, "Swagger Petstore");
    assert_eq!(model.version, "1.0.0");
    let ids: Vec<_> = model
        .operations
        .iter()
        .map(|op| {
            (
                op.method.as_str(),
                op.path.as_str(),
                op.operation_id.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        ids,
        [
            ("GET", "/pets", Some("listPets")),
            ("POST", "/pets", Some("createPet")),
            ("GET", "/pets/{petId}", Some("showPetById")),
            ("DELETE", "/pets/{petId}", Some("deletePet")),
            ("GET", "/store/inventory", Some("getInventory")),
        ]
    );
    let list = &model.operations[0];
    assert_eq!(list.pointer, "/paths/~1pets/get");
    let params: Vec<_> = list
        .parameters
        .iter()
        .map(|p| (p.name.as_str(), p.location, p.required))
        .collect();
    assert_eq!(
        params,
        [
            ("limit", ParameterLocation::Query, false),
            ("X-Request-Id", ParameterLocation::Header, false),
        ]
    );
    let statuses: Vec<_> = list.responses.iter().map(|r| r.status.as_str()).collect();
    assert_eq!(statuses, ["200", "default"]);
    assert_eq!(list.responses[0].headers, ["x-next"]);
    assert_eq!(
        list.responses[1].description.as_deref(),
        Some("unexpected error")
    );

    let create = &model.operations[1];
    let body = create.request_body.as_ref().unwrap();
    assert!(body.required);
    assert_eq!(body.content[0].media_type, "application/json");
    assert_eq!(
        body.content[0].example,
        Some(json!({"name": "Rex", "age": 3}))
    );

    let show = &model.operations[2];
    assert_eq!(show.parameters[0].name, "petId");
    assert_eq!(
        show.responses[0].content[0].examples,
        [(
            "rex".to_owned(),
            json!({"id": 1, "name": "Rex", "tag": null})
        )]
    );
}

#[tokio::test]
async fn security_servers_and_tags() {
    let model = load("petstore-3.1.json").await.model;
    assert_eq!(model.security, [vec![("bearerAuth".to_owned(), vec![])]]);
    assert_eq!(model.operations[0].security, None);
    assert_eq!(model.operations[3].security, Some(vec![]));
    assert_eq!(
        model.operations[4].security,
        Some(vec![vec![("apiKey".to_owned(), vec![])]])
    );
    assert_eq!(
        model.security_schemes,
        [
            (
                "bearerAuth".to_owned(),
                SecurityScheme::HttpBearer {
                    format: Some("JWT".into())
                }
            ),
            (
                "apiKey".to_owned(),
                SecurityScheme::ApiKey {
                    name: "X-API-Key".into(),
                    location: ParameterLocation::Header
                }
            ),
        ]
    );
    assert_eq!(model.servers[0].url, "https://petstore.example.com/v1");
    assert_eq!(model.servers[0].description.as_deref(), Some("Production"));
    let tags: Vec<_> = model.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tags, ["pets", "store"]);
}
