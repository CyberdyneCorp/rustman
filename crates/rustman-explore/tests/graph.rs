//! Task 3.1: the dependency graph from `links`, the producer heuristics and
//! `[explore.producers]` overrides.

mod common;

use std::collections::BTreeMap;

use common::load;
use rustman_explore::explore::graph::{EdgeSource, Graph, Unresolved};

async fn graph(overrides: &[(&str, &str)]) -> Graph {
    let model = load("explore/crud.yaml").await.model;
    let overrides: BTreeMap<String, String> = overrides
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Graph::build(&model, &overrides)
}

fn producers(graph: &Graph, consumer: &str, param: &str) -> Vec<(String, String, EdgeSource)> {
    graph
        .producers(consumer, param)
        .map(|e| (e.producer.clone(), e.pointer.clone(), e.source))
        .collect()
}

fn edge(producer: &str, pointer: &str, source: EdgeSource) -> (String, String, EdgeSource) {
    (producer.to_owned(), pointer.to_owned(), source)
}

#[tokio::test]
async fn post_produces_the_id_of_the_item_operations() {
    let graph = graph(&[]).await;
    for consumer in ["GET /users/{id}", "PUT /users/{id}", "DELETE /users/{id}"] {
        assert_eq!(
            producers(&graph, consumer, "id"),
            [
                edge("POST /users", "/id", EdgeSource::Heuristic),
                edge("GET /users", "/0/id", EdgeSource::Heuristic),
            ],
            "{consumer}"
        );
    }
}

#[tokio::test]
async fn nested_resources_have_a_producer_per_parameter() {
    let graph = graph(&[]).await;
    assert_eq!(
        producers(&graph, "GET /users/{id}/posts/{postId}", "postId"),
        [edge(
            "POST /users/{id}/posts",
            "/postId",
            EdgeSource::Heuristic
        )]
    );
    assert_eq!(
        producers(&graph, "GET /users/{id}/posts/{postId}", "id")[0],
        edge("POST /users", "/id", EdgeSource::Heuristic)
    );
}

#[tokio::test]
async fn links_name_producers_the_heuristic_cannot_see() {
    let graph = graph(&[]).await;
    assert_eq!(
        producers(&graph, "GET /teams/{slug}", "slug"),
        [edge("POST /teams", "/handle", EdgeSource::Link)]
    );
}

#[tokio::test]
async fn a_parameter_without_producer_is_unresolved() {
    let graph = graph(&[]).await;
    assert!(producers(&graph, "GET /orders/{orderId}", "orderId").is_empty());
    assert_eq!(
        graph.unresolved,
        [Unresolved {
            operation: "GET /orders/{orderId}".into(),
            parameter: "orderId".into(),
        }]
    );
}

#[tokio::test]
async fn overrides_pin_a_producer_first() {
    let graph = graph(&[("orderId", "createUser#/name"), ("slug", "GET /users")]).await;
    assert_eq!(
        producers(&graph, "GET /orders/{orderId}", "orderId"),
        [edge("POST /users", "/name", EdgeSource::Override)]
    );
    assert!(graph.unresolved.is_empty());
    assert_eq!(
        producers(&graph, "GET /teams/{slug}", "slug")[0],
        edge("GET /users", "/id", EdgeSource::Override)
    );
    let unknown = self::graph(&[("orderId", "nope")]).await;
    assert_eq!(
        unknown.warnings,
        ["[explore.producers] orderId: no operation 'nope'"]
    );
    assert_eq!(unknown.unresolved.len(), 1);
}

#[tokio::test]
async fn the_rendered_graph() {
    insta::assert_snapshot!("crud-graph", graph(&[]).await.render());
}
