/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

use std::collections::BTreeMap;

use super::*;
use crate::collector::metrics_ids::{
    SERVICE_MODE_GLOBAL, SERVICE_MODE_GLOBAL_JOB, SERVICE_MODE_REPLICATED,
    SERVICE_MODE_REPLICATED_JOB,
};
use crate::docker::types::{
    JobStatus, Placement, Present, ReplicatedService, RestartPolicy, ServiceMode, ServiceSpec,
    TaskSpec, Version,
};

fn with_mode(mode: ServiceMode) -> Service {
    Service {
        spec: ServiceSpec {
            mode,
            ..ServiceSpec::default()
        },
        ..Service::default()
    }
}

fn replicated(replicas: u64) -> ServiceMode {
    ServiceMode {
        replicated: Some(ReplicatedService {
            replicas: Some(replicas),
        }),
        ..ServiceMode::default()
    }
}

fn named(name: &str, mode: ServiceMode) -> Service {
    let mut service = with_mode(mode);
    service.spec.name = name.to_owned();
    service
}

/// Go: `TestServiceMode`
#[test]
fn service_mode_of_each_spec() {
    assert_eq!(
        service_mode(&with_mode(replicated(3))),
        SERVICE_MODE_REPLICATED
    );
    let global = ServiceMode {
        global: Some(Present {}),
        ..ServiceMode::default()
    };
    assert_eq!(service_mode(&with_mode(global)), SERVICE_MODE_GLOBAL);
    let replicated_job = ServiceMode {
        replicated_job: Some(ReplicatedJob::default()),
        ..ServiceMode::default()
    };
    assert_eq!(
        service_mode(&with_mode(replicated_job)),
        SERVICE_MODE_REPLICATED_JOB
    );
    let global_job = ServiceMode {
        global_job: Some(Present {}),
        ..ServiceMode::default()
    };
    assert_eq!(
        service_mode(&with_mode(global_job)),
        SERVICE_MODE_GLOBAL_JOB
    );
    // No mode at all is global, Go's default branch.
    assert_eq!(service_mode(&Service::default()), SERVICE_MODE_GLOBAL);
}

/// Go: `TestIsJobMode`
#[test]
fn is_job_mode_only_for_jobs() {
    for (mode, want) in [
        (SERVICE_MODE_REPLICATED, false),
        (SERVICE_MODE_GLOBAL, false),
        (SERVICE_MODE_REPLICATED_JOB, true),
        (SERVICE_MODE_GLOBAL_JOB, true),
        ("", false),
    ] {
        assert_eq!(is_job_mode(mode), want, "{mode:?}");
    }
}

/// Go: `TestJobTotalCompletions_Defaults`
#[test]
fn job_total_completions_defaults() {
    let job = |max_concurrent, total_completions| ReplicatedJob {
        max_concurrent,
        total_completions,
    };
    for (name, job, want) in [
        ("neither set", job(None, None), 1.0),
        ("max concurrent only", job(Some(4), None), 4.0),
        ("total completions only", job(None, Some(7)), 7.0),
        ("both set", job(Some(4), Some(7)), 7.0),
    ] {
        assert!(job_total_completions(&job).eq(&want), "{name}");
    }
}

/// Go: `TestBuildMetadata_ReplicatedJob`
#[test]
fn build_metadata_replicated_job() {
    let mut service = named(
        "job",
        ServiceMode {
            replicated_job: Some(ReplicatedJob {
                max_concurrent: Some(2),
                total_completions: Some(5),
            }),
            ..ServiceMode::default()
        },
    );
    service.job_status = Some(JobStatus {
        job_iteration: Version { index: 42 },
    });
    let metadata = build_metadata(&service, &CustomLabels::default());
    assert_eq!(metadata.service_mode, SERVICE_MODE_REPLICATED_JOB);
    assert!(metadata.configured_replicas.eq(&5.0), "TotalCompletions");
    assert_eq!(metadata.job_iteration, 42);
}

/// Go: `TestBuildMetadata_GlobalJob`
#[test]
fn build_metadata_global_job() {
    let mut service = named(
        "job",
        ServiceMode {
            global_job: Some(Present {}),
            ..ServiceMode::default()
        },
    );
    service.job_status = Some(JobStatus {
        job_iteration: Version { index: 9 },
    });
    let metadata = build_metadata(&service, &CustomLabels::default());
    assert_eq!(metadata.service_mode, SERVICE_MODE_GLOBAL_JOB);
    assert!(metadata.configured_replicas.eq(&0.0), "0 for global-job");
    assert_eq!(metadata.job_iteration, 9);
    // Without JobStatus (not a job, or not reported) the iteration stays 0.
    service.job_status = None;
    assert_eq!(
        build_metadata(&service, &CustomLabels::default()).job_iteration,
        0
    );
}

/// Go: `TestShortServiceName`
#[test]
fn short_service_name_drops_the_stack_prefix() {
    for (stack, full_name, want) in [
        ("", "myapp", "myapp"),
        ("mystack", "mystack_web", "web"),
        ("mystack", "other_web", "other_web"),
        ("mystack", "mystack_", ""),
        ("mystack", "mystack", "mystack"),
    ] {
        assert_eq!(
            short_service_name(stack, full_name),
            want,
            "{stack:?} {full_name:?}"
        );
    }
}

/// Go: `TestDisplayName`
#[test]
fn display_name_joins_stack_and_service() {
    for (stack, service, want) in [
        ("", "myservice", "myservice"),
        ("mystack", "mystack", "mystack"),
        ("mystack", "web", "mystack web"),
        ("", "", ""),
    ] {
        assert_eq!(display_name(stack, service), want, "{stack:?} {service:?}");
    }
}

/// Go: `TestBuildMetadata_StackAndService`
#[test]
fn build_metadata_stack_and_service() {
    let mut service = named("mystack_web", replicated(2));
    service.spec.labels =
        BTreeMap::from([(STACK_NAMESPACE_LABEL.to_owned(), "mystack".to_owned())]);
    let metadata = build_metadata(&service, &CustomLabels::default());
    assert_eq!(metadata.stack, "mystack");
    assert_eq!(metadata.service, "web");
    assert_eq!(metadata.service_mode, SERVICE_MODE_REPLICATED);
    assert!(metadata.configured_replicas.eq(&2.0));
}

/// Go: `TestBuildMetadata_PlacementCopied`; the copy is an owned clone, so a later change to the
/// service cannot reach the metadata.
#[test]
fn build_metadata_copies_the_placement() {
    let mut service = named("svc", replicated(1));
    service.spec.task_template = TaskSpec {
        placement: Some(Placement {
            constraints: vec!["node.role == manager".to_owned()],
            platforms: vec![Platform {
                architecture: "amd64".to_owned(),
                os: "linux".to_owned(),
            }],
        }),
        ..TaskSpec::default()
    };
    let metadata = build_metadata(&service, &CustomLabels::default());
    assert_eq!(metadata.constraints, ["node.role == manager"]);
    if let Some(placement) = service.spec.task_template.placement.as_mut() {
        placement.constraints[0] = "mutated".to_owned();
    }
    assert_eq!(metadata.constraints, ["node.role == manager"]);
    assert_eq!(metadata.platforms.len(), 1);
    assert_eq!(metadata.platforms[0].os, "linux");
}

/// Go: `TestBuildMetadata_RestartConditionNone`
#[test]
fn build_metadata_restart_condition_none() {
    let mut service = named("oneshot", replicated(1));
    service.spec.task_template.restart_policy = Some(RestartPolicy {
        condition: "none".to_owned(),
    });
    assert!(build_metadata(&service, &CustomLabels::default()).restart_condition_none);
    service.spec.task_template.restart_policy = Some(RestartPolicy {
        condition: "any".to_owned(),
    });
    assert!(!build_metadata(&service, &CustomLabels::default()).restart_condition_none);
    service.spec.task_template.restart_policy = None;
    assert!(!build_metadata(&service, &CustomLabels::default()).restart_condition_none);
}

/// Go: `TestBuildMetadata_CustomLabels`; the labels are an argument instead of Go's package
/// variable.
#[test]
fn build_metadata_custom_labels() {
    let custom = CustomLabels::new(
        &["team".to_owned(), "tier".to_owned()],
        &["team".to_owned(), "tier".to_owned()],
    );
    let mut service = named("svc", replicated(1));
    service.spec.labels = BTreeMap::from([("team".to_owned(), "platform".to_owned())]);
    let metadata = build_metadata(&service, &custom);
    assert_eq!(metadata.custom_labels["team"], "platform");
    assert_eq!(
        metadata.custom_labels["tier"], "",
        "an absent label is empty"
    );
    assert_eq!(custom.sanitized_names(), ["team", "tier"]);
}

/// Go: `TestSetServiceMetadata_PreservesDesiredReplicas`
#[test]
fn set_service_metadata_preserves_desired_replicas() {
    let mut cache = MetadataCache::new();
    let metadata = || ServiceMetadata {
        stack: "stack".to_owned(),
        service: "svc".to_owned(),
        service_mode: SERVICE_MODE_REPLICATED.to_owned(),
        ..ServiceMetadata::default()
    };
    cache.set("svc1", metadata());
    cache.set_desired_replicas("svc1", 5.0);
    // A service update stores fresh metadata; the desired replicas survive it.
    cache.set("svc1", metadata());
    assert!(cache.get("svc1").unwrap().desired_replicas.eq(&5.0));
}

#[test]
fn metadata_cache_replaces_deletes_and_copies() {
    let mut cache = MetadataCache::new();
    cache.set_desired_replicas("unknown", 3.0);
    assert!(
        cache.get("unknown").is_none(),
        "an unknown service is left alone"
    );
    cache.set("svc1", ServiceMetadata::default());
    cache.set_desired_replicas("svc1", 5.0);
    cache.replace(
        "svc1",
        ServiceMetadata {
            desired_replicas: 2.0,
            ..ServiceMetadata::default()
        },
    );
    assert!(cache.get("svc1").unwrap().desired_replicas.eq(&2.0));
    let copy = cache.all();
    assert_eq!((cache.ids(), cache.len()), (vec!["svc1".to_owned()], 1));
    cache.delete("svc1");
    assert!(cache.get("svc1").is_none());
    assert!(cache.is_empty());
    assert_eq!(copy.len(), 1, "the copy is a snapshot");
}

#[test]
fn node_cache_holds_a_copy() {
    let mut cache = NodeCache::new();
    assert!(cache.get().is_empty());
    let nodes = vec![Node {
        id: "n1".to_owned(),
        ..Node::default()
    }];
    cache.set(&nodes);
    assert_eq!(cache.get(), nodes);
    assert_eq!(&*cache.shared(), nodes);
}

/// Go dereferences the missing pointer; the daemon always sets it.
#[test]
#[should_panic(expected = "has no Replicas")]
fn build_metadata_panics_on_a_replicated_service_without_replicas() {
    let service = with_mode(ServiceMode {
        replicated: Some(ReplicatedService { replicas: None }),
        ..ServiceMode::default()
    });
    let _ = build_metadata(&service, &CustomLabels::default());
}
