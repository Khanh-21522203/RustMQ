use crate::broker::controller::types::{
    BrokerRegistration, ControllerCommand, ControllerMetadata, PartitionRecord, TopicRecord,
};
use std::collections::HashSet;

/// Apply a `ControllerCommand` to the controller metadata state machine.
pub fn apply_controller_command(meta: &mut ControllerMetadata, cmd: ControllerCommand) {
    match cmd {
        ControllerCommand::CreateTopic {
            topic,
            num_partitions,
            replication_factor,
        } => {
            meta.topics.entry(topic.clone()).or_insert(TopicRecord {
                num_partitions,
                replication_factor,
            });
            for p in 0..num_partitions {
                meta.partitions
                    .entry((topic.clone(), p))
                    .or_insert(PartitionRecord {
                        leader: 0,
                        isr: Vec::new(),
                        replicas: Vec::new(),
                        leader_epoch: 0,
                    });
            }
        }
        ControllerCommand::DeleteTopic { topic } => {
            if let Some(record) = meta.topics.remove(&topic) {
                for p in 0..record.num_partitions {
                    meta.partitions.remove(&(topic.clone(), p));
                }
            }
        }
        ControllerCommand::PartitionChange {
            topic,
            partition,
            leader,
            isr,
            replicas,
        } => {
            let entry = meta
                .partitions
                .entry((topic, partition))
                .or_insert(PartitionRecord {
                    leader: 0,
                    isr: Vec::new(),
                    replicas: Vec::new(),
                    leader_epoch: 0,
                });
            let leader_changed = entry.leader != leader;
            entry.leader = leader;
            entry.isr = isr;
            entry.replicas = replicas;
            if leader_changed {
                entry.leader_epoch += 1;
            }
        }
        ControllerCommand::RegisterBroker {
            broker_id,
            api_addr,
            rpc_addr,
        } => {
            meta.brokers.insert(
                broker_id,
                BrokerRegistration {
                    broker_id,
                    api_addr,
                    rpc_addr,
                    last_seen_ms: 0,
                },
            );
        }
        ControllerCommand::UnregisterBroker { broker_id } => {
            meta.brokers.remove(&broker_id);
        }
        ControllerCommand::BumpControllerEpoch => {
            meta.controller_epoch += 1;
        }
        ControllerCommand::BrokerHeartbeat {
            broker_id,
            timestamp_ms,
        } => {
            if let Some(reg) = meta.brokers.get_mut(&broker_id) {
                // Monotonically advance — never move backward.
                if timestamp_ms > reg.last_seen_ms {
                    reg.last_seen_ms = timestamp_ms;
                }
            }
        }
        ControllerCommand::MarkBrokerDead {
            broker_id,
            new_assignments,
        } => {
            meta.brokers.remove(&broker_id);
            for assign in new_assignments {
                let entry = meta
                    .partitions
                    .entry((assign.topic, assign.partition))
                    .or_insert(PartitionRecord {
                        leader: 0,
                        isr: Vec::new(),
                        replicas: Vec::new(),
                        leader_epoch: 0,
                    });
                let leader_changed = entry.leader != assign.new_leader;
                entry.leader = assign.new_leader;
                entry.isr = assign.new_isr;
                entry.replicas = assign.replicas;
                if leader_changed {
                    entry.leader_epoch += 1;
                }
            }
        }
    }
}

/// Given the current metadata and a dead broker, compute the new
/// `PartitionAssignment` list for all partitions that were led by that broker.
/// - New leader: first alive ISR member (i.e. `alive_brokers` contains it).
/// - If no alive ISR member → leader = 0 (OFFLINE).
pub fn compute_failover_assignments(
    meta: &ControllerMetadata,
    dead_broker_id: u64,
) -> Vec<crate::broker::controller::types::PartitionAssignment> {
    let alive: HashSet<u64> = meta
        .brokers
        .keys()
        .copied()
        .filter(|&id| id != dead_broker_id)
        .collect();

    meta.partitions
        .iter()
        .filter(|(_, pr)| pr.leader == dead_broker_id)
        .map(|((topic, partition), pr)| {
            let new_isr: Vec<u64> = pr
                .isr
                .iter()
                .copied()
                .filter(|&id| id != dead_broker_id && alive.contains(&id))
                .collect();
            let new_leader = new_isr.first().copied().unwrap_or(0);
            crate::broker::controller::types::PartitionAssignment {
                topic: topic.clone(),
                partition: *partition,
                new_leader,
                new_isr,
                replicas: pr.replicas.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_topic_initializes_partitions() {
        let mut meta = ControllerMetadata::default();
        apply_controller_command(
            &mut meta,
            ControllerCommand::CreateTopic {
                topic: "events".to_string(),
                num_partitions: 3,
                replication_factor: 1,
            },
        );
        assert_eq!(meta.topics.len(), 1);
        assert_eq!(meta.partitions.len(), 3);
        assert!(meta.partitions.contains_key(&("events".to_string(), 0)));
        assert!(meta.partitions.contains_key(&("events".to_string(), 2)));
        assert_eq!(meta.topics["events"].num_partitions, 3);
        assert_eq!(meta.topics["events"].replication_factor, 1);
    }

    #[test]
    fn create_topic_is_idempotent() {
        let mut meta = ControllerMetadata::default();
        for _ in 0..3 {
            apply_controller_command(
                &mut meta,
                ControllerCommand::CreateTopic {
                    topic: "t".to_string(),
                    num_partitions: 2,
                    replication_factor: 1,
                },
            );
        }
        assert_eq!(meta.topics.len(), 1);
        assert_eq!(meta.partitions.len(), 2);
    }

    #[test]
    fn delete_topic_removes_all_partitions() {
        let mut meta = ControllerMetadata::default();
        apply_controller_command(
            &mut meta,
            ControllerCommand::CreateTopic {
                topic: "events".to_string(),
                num_partitions: 2,
                replication_factor: 1,
            },
        );
        apply_controller_command(
            &mut meta,
            ControllerCommand::DeleteTopic {
                topic: "events".to_string(),
            },
        );
        assert!(meta.topics.is_empty());
        assert!(meta.partitions.is_empty());
    }

    #[test]
    fn delete_unknown_topic_is_noop() {
        let mut meta = ControllerMetadata::default();
        apply_controller_command(
            &mut meta,
            ControllerCommand::DeleteTopic {
                topic: "nonexistent".to_string(),
            },
        );
        assert!(meta.topics.is_empty());
    }

    #[test]
    fn partition_change_bumps_epoch_only_on_leader_change() {
        let mut meta = ControllerMetadata::default();
        apply_controller_command(
            &mut meta,
            ControllerCommand::CreateTopic {
                topic: "t".to_string(),
                num_partitions: 1,
                replication_factor: 3,
            },
        );

        // Assign initial leader
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 0,
                leader: 1,
                isr: vec![1, 2, 3],
                replicas: vec![1, 2, 3],
            },
        );
        let p = &meta.partitions[&("t".to_string(), 0)];
        assert_eq!(p.leader, 1);
        assert_eq!(p.leader_epoch, 1);

        // ISR shrink, same leader — epoch must NOT bump
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 0,
                leader: 1,
                isr: vec![1, 2],
                replicas: vec![1, 2, 3],
            },
        );
        let p = &meta.partitions[&("t".to_string(), 0)];
        assert_eq!(p.leader_epoch, 1);
        assert_eq!(p.isr, vec![1, 2]);

        // Leader failover — epoch bumps
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 0,
                leader: 2,
                isr: vec![2, 3],
                replicas: vec![1, 2, 3],
            },
        );
        let p = &meta.partitions[&("t".to_string(), 0)];
        assert_eq!(p.leader, 2);
        assert_eq!(p.leader_epoch, 2);
    }

    #[test]
    fn register_and_unregister_broker() {
        let mut meta = ControllerMetadata::default();
        apply_controller_command(
            &mut meta,
            ControllerCommand::RegisterBroker {
                broker_id: 1,
                api_addr: "127.0.0.1:9092".to_string(),
                rpc_addr: "127.0.0.1:9093".to_string(),
            },
        );
        assert_eq!(meta.brokers.len(), 1);
        assert_eq!(meta.brokers[&1].api_addr, "127.0.0.1:9092");

        apply_controller_command(
            &mut meta,
            ControllerCommand::UnregisterBroker { broker_id: 1 },
        );
        assert!(meta.brokers.is_empty());
    }

    #[test]
    fn bump_controller_epoch() {
        let mut meta = ControllerMetadata::default();
        assert_eq!(meta.controller_epoch, 0);
        apply_controller_command(&mut meta, ControllerCommand::BumpControllerEpoch);
        apply_controller_command(&mut meta, ControllerCommand::BumpControllerEpoch);
        assert_eq!(meta.controller_epoch, 2);
    }

    fn registered_broker(id: u64) -> BrokerRegistration {
        BrokerRegistration {
            broker_id: id,
            api_addr: format!("127.0.0.1:{}", 9000 + id),
            rpc_addr: format!("127.0.0.1:{}", 9100 + id),
            last_seen_ms: 0,
        }
    }

    #[test]
    fn broker_heartbeat_updates_last_seen_ms() {
        let mut meta = ControllerMetadata::default();
        meta.brokers.insert(1, registered_broker(1));

        apply_controller_command(
            &mut meta,
            ControllerCommand::BrokerHeartbeat {
                broker_id: 1,
                timestamp_ms: 1000,
            },
        );
        assert_eq!(meta.brokers[&1].last_seen_ms, 1000);

        // Later timestamp advances.
        apply_controller_command(
            &mut meta,
            ControllerCommand::BrokerHeartbeat {
                broker_id: 1,
                timestamp_ms: 2000,
            },
        );
        assert_eq!(meta.brokers[&1].last_seen_ms, 2000);

        // Earlier timestamp is ignored (monotone).
        apply_controller_command(
            &mut meta,
            ControllerCommand::BrokerHeartbeat {
                broker_id: 1,
                timestamp_ms: 500,
            },
        );
        assert_eq!(meta.brokers[&1].last_seen_ms, 2000);
    }

    #[test]
    fn broker_heartbeat_for_unknown_broker_is_noop() {
        let mut meta = ControllerMetadata::default();
        // Should not panic.
        apply_controller_command(
            &mut meta,
            ControllerCommand::BrokerHeartbeat {
                broker_id: 99,
                timestamp_ms: 1000,
            },
        );
        assert!(meta.brokers.is_empty());
    }

    #[test]
    fn mark_broker_dead_removes_broker_and_reassigns_partitions() {
        use crate::broker::controller::types::PartitionAssignment;
        let mut meta = ControllerMetadata::default();
        meta.brokers.insert(1, registered_broker(1));
        meta.brokers.insert(2, registered_broker(2));

        // Partition led by broker 1, ISR has both.
        apply_controller_command(
            &mut meta,
            ControllerCommand::CreateTopic {
                topic: "t".to_string(),
                num_partitions: 1,
                replication_factor: 2,
            },
        );
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 0,
                leader: 1,
                isr: vec![1, 2],
                replicas: vec![1, 2],
            },
        );
        assert_eq!(meta.partitions[&("t".to_string(), 0)].leader_epoch, 1);

        apply_controller_command(
            &mut meta,
            ControllerCommand::MarkBrokerDead {
                broker_id: 1,
                new_assignments: vec![PartitionAssignment {
                    topic: "t".to_string(),
                    partition: 0,
                    new_leader: 2,
                    new_isr: vec![2],
                    replicas: vec![1, 2],
                }],
            },
        );

        assert!(!meta.brokers.contains_key(&1));
        let pr = &meta.partitions[&("t".to_string(), 0)];
        assert_eq!(pr.leader, 2);
        assert_eq!(pr.isr, vec![2]);
        assert_eq!(pr.leader_epoch, 2); // bumped because leader changed
    }

    #[test]
    fn compute_failover_assigns_correct_new_leader() {
        let mut meta = ControllerMetadata::default();
        meta.brokers.insert(1, registered_broker(1));
        meta.brokers.insert(2, registered_broker(2));
        meta.brokers.insert(3, registered_broker(3));

        apply_controller_command(
            &mut meta,
            ControllerCommand::CreateTopic {
                topic: "t".to_string(),
                num_partitions: 2,
                replication_factor: 3,
            },
        );
        // p0: led by 1, ISR [1,2,3]
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 0,
                leader: 1,
                isr: vec![1, 2, 3],
                replicas: vec![1, 2, 3],
            },
        );
        // p1: led by 2 — should NOT be in the result (not led by dead broker 1)
        apply_controller_command(
            &mut meta,
            ControllerCommand::PartitionChange {
                topic: "t".to_string(),
                partition: 1,
                leader: 2,
                isr: vec![2, 3],
                replicas: vec![1, 2, 3],
            },
        );

        let assignments = compute_failover_assignments(&meta, 1);
        assert_eq!(assignments.len(), 1);
        let a = &assignments[0];
        assert_eq!(a.partition, 0);
        // ISR after removing dead broker: [2, 3]; first is new leader
        assert_eq!(a.new_leader, 2);
        assert_eq!(a.new_isr, vec![2, 3]);
    }
}
