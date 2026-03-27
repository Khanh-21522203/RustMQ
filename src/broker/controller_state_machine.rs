use crate::broker::controller_types::{
    BrokerRegistration, ControllerCommand, ControllerMetadata, PartitionRecord, TopicRecord,
};

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
                },
            );
        }
        ControllerCommand::UnregisterBroker { broker_id } => {
            meta.brokers.remove(&broker_id);
        }
        ControllerCommand::BumpControllerEpoch => {
            meta.controller_epoch += 1;
        }
    }
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
}
