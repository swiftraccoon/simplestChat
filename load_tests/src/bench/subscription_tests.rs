//! The retained JS implementation independently judges the actual Rust graph.

use super::{options, policy_tests, subscription};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::Path;

fn fixture(mode: &str, kind: &str) -> (options::Options, options::Scenario, Value, Value) {
    let mut args = policy_tests::required();
    args.extend([
        "--clients".to_owned(),
        "6".to_owned(),
        "--scenarios".to_owned(),
        kind.to_owned(),
        "--subscription-plan".to_owned(),
        mode.to_owned(),
        "--subscription-seed".to_owned(),
        "17".to_owned(),
        "--duration".to_owned(),
        "8".to_owned(),
    ]);
    let args: Vec<_> = args.into_iter().map(OsString::from).collect();
    let options = options::parse(&args, Path::new("/owned/root")).unwrap();
    let scenario = options.scenarios[0].clone();
    let plan = subscription::expected(&(&options).into(), &scenario).unwrap();
    let summary = json!({"run":{"configuration":{
        "subscriptionPlan":mode,"subscriptionSeed":17,"numClients":scenario.clients,
        "numRooms":scenario.rooms,"roomId":"owned-local-room","publishRatio":1,"churnRate":0,
        "durationSecs":options.duration,"warmupSecs":options.warmup,"rampUpSecs":options.ramp_up,
        "maxAudioConsumers":4,"maxVideoConsumers":4,"mediaConfig":{"audioEnabled":true,"videoEnabled":kind != "audio"}
    }},"subscriptionPlan":plan});
    let clients: Vec<_> = plan.clients.iter().map(|client| {
        let mut realized = Vec::new();
        for (kind, targets) in [("audio", &client.targets.audio), ("video", &client.targets.video)] {
            for target in targets {
                realized.push(json!({"publisherClientId":target,"producerId":format!("{target}-{kind}"),
                    "consumerId":format!("{}-{target}-{kind}",client.client_id),"isAudio":kind == "audio"}));
            }
        }
        let delivery: Vec<_> = realized.iter().map(|edge| json!({
            "consumerId":edge["consumerId"],"producerId":edge["producerId"],"isAudio":edge["isAudio"],
            "attempt":1,"passed":true,"skippedShortLived":false,"eligibleSeconds":options.duration,
            "secondsWithPackets":options.duration,"longestGapSeconds":0,"packetsBySecond":vec![50;options.duration as usize]
        })).collect();
        json!({"clientId":client.client_id,"roomId":if scenario.rooms == 1 { "owned-local-room".to_owned() }
            else { format!("owned-local-room-{}",client.room) },
            "connectionAttempts":[{"coverage":{"passed":true,"skippedShortTail":false,
                "expectedAudio":client.targets.audio.len(),"expectedVideo":client.targets.video.len(),
                "plannedSubscriptions":{"targets":client.targets,"realized":realized,"passed":true}}}],
            "consumerDelivery":delivery})
    }).collect();
    (options, scenario, summary, json!(clients))
}

#[test]
fn actual_planner_and_delivery_proofs_match_js_across_both_graphs_and_hostile_evidence() {
    let mut cases = Vec::new();
    for mode in ["ring-v1", "hotspot-v1"] {
        for kind in ["conference", "multi-room", "audio"] {
            let (mut options, scenario, summary, results) = fixture(mode, kind);
            for seed in [0, 17, 29, u32::MAX] {
                options.subscription_seed = Some(seed);
                cases.push(json!({"op":"expectedSubscriptionPlan","args":[options,scenario]}));
            }
            options.subscription_seed = Some(17);
            cases.push(
                json!({"op":"verifySubscriptionReport","args":[summary,results,options,scenario]}),
            );
            let mut reversed = results.as_array().unwrap().clone();
            reversed.reverse();
            cases.push(
                json!({"op":"verifySubscriptionReport","args":[summary,reversed,options,scenario]}),
            );
            for mutation in [
                "passed-flag",
                "missing-delivery",
                "duplicate-delivery",
                "wrong-target",
                "native-producer-conflict",
                "native-consumer-conflict",
                "wrong-kind",
                "wrong-attempt",
                "skipped-tail",
                "short-eligible",
                "missing-second",
                "packet-zero",
                "packet-unsafe",
                "wrong-plan-hash",
                "wrong-seed",
                "wrong-duration",
                "wrong-room",
                "duplicate-client",
                "extra-native-private-fields",
                "missing-coverage",
                "missing-realized",
            ] {
                let mut summary = summary.clone();
                let mut results = results.clone();
                match mutation {
                    "passed-flag" => {
                        results[0]["connectionAttempts"][0]["coverage"]["passed"] = json!(false)
                    }
                    "missing-delivery" => {
                        results[0]["consumerDelivery"].as_array_mut().unwrap().pop();
                    }
                    "duplicate-delivery" => {
                        let extra = results[0]["consumerDelivery"][0].clone();
                        results[0]["consumerDelivery"]
                            .as_array_mut()
                            .unwrap()
                            .push(extra);
                    }
                    "wrong-target" => {
                        results[0]["connectionAttempts"][0]["coverage"]["plannedSubscriptions"]["realized"]
                            [0]["publisherClientId"] = json!("PRIVATE")
                    }
                    "native-producer-conflict" => {
                        let producer = results[0]["consumerDelivery"][0]["producerId"].clone();
                        results[1]["consumerDelivery"][0]["producerId"] = producer.clone();
                        results[1]["connectionAttempts"][0]["coverage"]["plannedSubscriptions"]["realized"]
                            [0]["producerId"] = producer;
                    }
                    "native-consumer-conflict" => {
                        let consumer = results[0]["consumerDelivery"][0]["consumerId"].clone();
                        results[1]["consumerDelivery"][0]["consumerId"] = consumer.clone();
                        results[1]["connectionAttempts"][0]["coverage"]["plannedSubscriptions"]["realized"]
                            [0]["consumerId"] = consumer;
                    }
                    "wrong-kind" => results[0]["consumerDelivery"][0]["isAudio"] = json!(false),
                    "wrong-attempt" => results[0]["consumerDelivery"][0]["attempt"] = json!(2),
                    "skipped-tail" => {
                        results[0]["consumerDelivery"][0]["skippedShortLived"] = json!(true)
                    }
                    "short-eligible" => {
                        results[0]["consumerDelivery"][0]["eligibleSeconds"] = json!(7)
                    }
                    "missing-second" => {
                        results[0]["consumerDelivery"][0]["packetsBySecond"]
                            .as_array_mut()
                            .unwrap()
                            .pop();
                    }
                    "packet-zero" => {
                        results[0]["consumerDelivery"][0]["packetsBySecond"][0] = json!(0)
                    }
                    "packet-unsafe" => {
                        results[0]["consumerDelivery"][0]["packetsBySecond"][0] =
                            json!(9_007_199_254_740_992_u64)
                    }
                    "wrong-plan-hash" => {
                        summary["subscriptionPlan"]["sha256"] = json!("f".repeat(64))
                    }
                    "wrong-seed" => summary["run"]["configuration"]["subscriptionSeed"] = json!(18),
                    "wrong-duration" => summary["run"]["configuration"]["durationSecs"] = json!(9),
                    "wrong-room" => results[0]["roomId"] = json!("PRIVATE"),
                    "duplicate-client" => results[0]["clientId"] = results[1]["clientId"].clone(),
                    "extra-native-private-fields" => {
                        results[0]["connectionAttempts"][0]["coverage"]["plannedSubscriptions"]["realized"]
                            [0]["private"] = json!("PRIVATE SDP")
                    }
                    "missing-coverage" => {
                        results[0]["connectionAttempts"][0]
                            .as_object_mut()
                            .unwrap()
                            .remove("coverage");
                    }
                    "missing-realized" => {
                        results[0]["connectionAttempts"][0]["coverage"]["plannedSubscriptions"]
                            .as_object_mut()
                            .unwrap()
                            .remove("realized");
                    }
                    _ => unreachable!(),
                }
                cases.push(json!({"op":"verifySubscriptionReport","args":[summary,results,options,scenario]}));
            }
        }
    }
    policy_tests::compare(&cases);
}
