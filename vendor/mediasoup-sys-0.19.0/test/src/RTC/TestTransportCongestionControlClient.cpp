#include "DepLibUV.hpp"
#include "RTC/TransportCongestionControlClient.hpp"
#include "mocks/include/MockShared.hpp"
#include <catch2/catch_test_macros.hpp>
#include <libwebrtc/modules/congestion_controller/goog_cc/goog_cc_network_control.h>
#include <libwebrtc/modules/remote_bitrate_estimator/aimd_rate_control.h>
#include <algorithm>
#include <array>
#include <functional>
#include <memory>
#include <utility>
#include <vector>

namespace
{
	webrtc::TargetRateConstraints Constraints(int64_t atMs, int64_t minimum, int64_t maximum)
	{
		webrtc::TargetRateConstraints constraints;
		constraints.at_time       = webrtc::Timestamp::ms(atMs);
		constraints.min_data_rate = webrtc::DataRate::bps(minimum);
		constraints.max_data_rate = webrtc::DataRate::bps(maximum);
		return constraints;
	}

	webrtc::StreamsConfig Streams(int64_t atMs, int64_t minimum, int64_t maximum)
	{
		webrtc::StreamsConfig streams;
		streams.at_time                     = webrtc::Timestamp::ms(atMs);
		streams.min_total_allocated_bitrate = webrtc::DataRate::bps(minimum);
		streams.max_padding_rate           = webrtc::DataRate::bps(maximum * 85 / 100);
		streams.max_total_allocated_bitrate = webrtc::DataRate::bps(maximum);
		return streams;
	}

	class PacketRouter final : public webrtc::PacketRouter
	{
	public:
		void SendPacket(RTC::RTP::Packet*, const webrtc::PacedPacketInfo& info) override
		{
			this->sent.emplace_back(info);
		}

		RTC::RTP::Packet* GeneratePadding(size_t size) override
		{
			return this->generator.GetNextPacket(size);
		}

		std::vector<webrtc::PacedPacketInfo> sent;
		RTC::RTP::ProbationGenerator generator;
	};

	class Observer final : public webrtc::TargetTransferRateObserver
	{
	public:
		void OnTargetTransferRate(webrtc::TargetTransferRate rate) override
		{
			this->rates.emplace_back(rate.target_rate.bps());
			++this->depth;
			this->maximumDepth = std::max(this->maximumDepth, this->depth);
			if (this->onRate)
			{
				this->onRate();
			}
			--this->depth;
		}

		std::function<void()> onRate;
		std::vector<int64_t> rates;
		size_t maximumDepth{ 0 };
		size_t depth{ 0 };
	};

	class Controller final : public webrtc::NetworkControllerInterface
	{
	public:
		webrtc::NetworkControlUpdate OnTargetRateConstraints(webrtc::TargetRateConstraints) override
		{
			return this->nextUpdate;
		}

		webrtc::NetworkControlUpdate OnNetworkAvailability(webrtc::NetworkAvailability) override { return {}; }
		webrtc::NetworkControlUpdate OnNetworkRouteChange(webrtc::NetworkRouteChange) override { return {}; }
		webrtc::NetworkControlUpdate OnProcessInterval(webrtc::ProcessInterval) override { return {}; }
		webrtc::NetworkControlUpdate OnRemoteBitrateReport(webrtc::RemoteBitrateReport) override { return {}; }
		webrtc::NetworkControlUpdate OnRoundTripTimeUpdate(webrtc::RoundTripTimeUpdate) override { return {}; }
		webrtc::NetworkControlUpdate OnSentPacket(webrtc::SentPacket) override { return {}; }
		webrtc::NetworkControlUpdate OnStreamsConfig(webrtc::StreamsConfig) override { return {}; }
		webrtc::NetworkControlUpdate OnTransportLossReport(webrtc::TransportLossReport) override { return {}; }
		webrtc::NetworkControlUpdate OnTransportPacketsFeedback(webrtc::TransportPacketsFeedback) override
		{
			return {};
		}
		webrtc::NetworkControlUpdate OnNetworkStateEstimate(webrtc::NetworkStateEstimate) override { return {}; }

		webrtc::NetworkControlUpdate nextUpdate;
	};

	class Factory final : public webrtc::NetworkControllerFactoryInterface
	{
	public:
		std::unique_ptr<webrtc::NetworkControllerInterface> Create(webrtc::NetworkControllerConfig) override
		{
			auto controller = std::make_unique<Controller>();
			this->controller = controller.get();
			return controller;
		}

		webrtc::TimeDelta GetProcessInterval() const override { return webrtc::TimeDelta::ms(25); }

		Controller* controller{ nullptr };
	};

	class ClientListener final : public RTC::TransportCongestionControlClient::Listener
	{
	public:
		void OnTransportCongestionControlClientBitrates(
		  RTC::TransportCongestionControlClient*, RTC::TransportCongestionControlClient::Bitrates& bitrates) override
		{
			this->rates.emplace_back(bitrates.availableBitrate);
		}
		void OnTransportCongestionControlClientSendRtpPacket(
		  RTC::TransportCongestionControlClient*, RTC::RTP::Packet*, const webrtc::PacedPacketInfo&) override
		{
		}

		std::vector<int64_t> rates;
	};

	class RecoveryConfig final : public webrtc::WebRtcKeyValueConfig
	{
	public:
		explicit RecoveryConfig(bool legacyAlrGrowth, std::string probeOptions = "")
		  : legacyAlrGrowth(legacyAlrGrowth), probeOptions(std::move(probeOptions)) {}
		std::string Lookup(absl::string_view key) const override
		{
			if (key == "WebRTC-Bwe-ProbingConfiguration")
			{
				return this->probeOptions;
			}
			if (key == "WebRTC-Bwe-AlrLimitedBackoff" ||
			    (this->legacyAlrGrowth && key == "WebRTC-DontIncreaseDelayBasedBweInAlr"))
			{
				return "Enabled";
			}
			return "";
		}

	private:
		const bool legacyAlrGrowth;
		const std::string probeOptions;
	};

	struct RecoveryResult
	{
		int64_t estimate{ 0 };
		int64_t maximumEstimate{ 0 };
		absl::optional<int64_t> firstAlrAt;
		absl::optional<int64_t> firstProbeAt;
		int64_t alrEstimate{ 0 };
		size_t periodicProbes{ 0 };
	};

	RecoveryResult ModelRecovery(
	  bool legacyAlrGrowth, int64_t initialRate, int64_t acknowledgedAndSentRate, int64_t allocatedDemand = 0)
	{
		RecoveryConfig config(legacyAlrGrowth);
		webrtc::AimdRateControl aimd(&config, /*send_side*/ true);
		webrtc::AlrDetector alr(&config);
		webrtc::ProbeController prober(&config);
		aimd.SetMinBitrate(webrtc::DataRate::bps(100000));
		aimd.SetStartBitrate(webrtc::DataRate::bps(initialRate));
		prober.EnablePeriodicAlrProbing(true);
		REQUIRE_FALSE(prober.SetBitrates(100000, initialRate, 3000000, 1000).empty());
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(allocatedDemand, 1100).empty());
		// Expire startup probing before considering subsequent recovery probes.
		REQUIRE(prober.Process(2001).empty());

		RecoveryResult result;
		int64_t unsentFraction{ 0 };
		absl::optional<int64_t> modeledAlrStart;
		for (int64_t atMs = 3000; atMs <= 63000; atMs += 50)
		{
			const bool wasInAlr = alr.GetApplicationLimitedRegionStartTime().has_value();
			aimd.SetInApplicationLimitedRegion(wasInAlr);
			webrtc::RateControlInput normal(
			  webrtc::BandwidthUsage::kBwNormal, webrtc::DataRate::bps(acknowledgedAndSentRate));
			const auto estimate = aimd.Update(&normal, webrtc::Timestamp::ms(atMs));
			result.estimate = estimate.bps();
			result.maximumEstimate = std::max(result.maximumEstimate, result.estimate);
			alr.SetEstimatedBitrate(static_cast<int>(result.estimate));
			// Preserve fractional bytes: this is a known, constant tracked send
			// rate, not an inference that every reported RTP byte was ALR tracked.
			unsentFraction += acknowledgedAndSentRate * 50;
			alr.OnBytesSent(static_cast<size_t>(unsentFraction / 8000), atMs);
			unsentFraction %= 8000;
			const bool inAlr = alr.GetApplicationLimitedRegionStartTime().has_value();
			if (inAlr && !modeledAlrStart)
			{
				modeledAlrStart = atMs;
				result.firstAlrAt = atMs;
				result.alrEstimate = result.estimate;
			}
			else if (!inAlr)
			{
				modeledAlrStart.reset();
			}
			// The pinned detector stamps ALR entry with the real DepLibUV clock.
			// Normalize only that timestamp to this test's modeled clock; retain
			// the detector's actual budget/entry/exit decisions and native timer.
			prober.SetAlrStartTimeMs(modeledAlrStart);
			auto probes = prober.SetEstimatedBitrate(result.estimate, atMs);
			const auto periodic = prober.Process(atMs);
			probes.insert(probes.end(), periodic.begin(), periodic.end());
			for (const auto& probe : probes)
			{
				REQUIRE((inAlr || allocatedDemand > result.estimate));
				REQUIRE(probe.target_data_rate.bps() > result.estimate);
				REQUIRE(probe.target_data_rate.bps() <= 3000000);
				++result.periodicProbes;
				if (!result.firstProbeAt)
				{
					result.firstProbeAt = atMs;
				}
			}
		}
		return result;
	}

	void InitializeDemandProbe(
	  webrtc::ProbeController& prober, int64_t demand = 2400000, int64_t estimate = 216143)
	{
		REQUIRE_FALSE(prober.SetBitrates(100000, estimate, 3000000, 1000).empty());
		// Install real demand while startup probing is pending, so an immediate
		// allocation-change probe cannot hide a missing periodic recovery probe.
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(demand, 1100).empty());
		REQUIRE(prober.Process(2001).empty());
	}
} // namespace

TEST_CASE("constraint updates reach the observer and native probe pacer", "[bandwidth-constraints]")
{
	PacketRouter router;
	Factory factory;
	Observer observer;
	webrtc::BitrateConstraints initial;
	initial.start_bitrate_bps = 600000;
	webrtc::RtpTransportControllerSend controller(&router, nullptr, &factory, initial);
	controller.RegisterTargetTransferRateObserver(&observer);
	controller.OnNetworkAvailability(true);

	webrtc::TargetTransferRate target;
	target.at_time     = webrtc::Timestamp::ms(1000);
	target.target_rate = webrtc::DataRate::bps(420000);
	factory.controller->nextUpdate.target_rate = target;
	webrtc::PacerConfig pacing;
	pacing.at_time     = target.at_time;
	pacing.time_window = webrtc::TimeDelta::seconds(1);
	pacing.data_window = webrtc::DataSize::bytes(2100000 / 8);
	pacing.pad_window  = webrtc::DataSize::bytes(125000 / 8);
	factory.controller->nextUpdate.pacer_config = pacing;
	webrtc::ProbeClusterConfig probe;
	probe.at_time          = target.at_time;
	probe.target_data_rate = webrtc::DataRate::bps(900000);
	probe.id               = 55;
	factory.controller->nextUpdate.probe_cluster_configs.emplace_back(probe);
	factory.controller->nextUpdate.congestion_window = webrtc::DataSize::Zero();

	// The worker sends stream allocation before its constraint update.
	controller.SetAllocatedSendBitrateLimits(100000, 2550000, 3000000);
	controller.SetClientBitratePreferences(Constraints(1000, 100000, 3000000));
	REQUIRE(observer.rates == std::vector<int64_t>{ 420000 });
	REQUIRE(controller.packet_sender()->GetPacingInfo().probe_cluster_id == 55);
	REQUIRE(controller.packet_sender()->GetPacingInfo().send_bitrate_bps == 900000);
	controller.packet_sender()->Process();
	REQUIRE(router.sent.empty());

	// A subsequent constraint response opens the actual pacer's window. The
	// previously delivered probe must now send without another probe request.
	factory.controller->nextUpdate = webrtc::NetworkControlUpdate();
	factory.controller->nextUpdate.congestion_window = webrtc::DataSize::Infinity();
	controller.SetClientBitratePreferences(Constraints(1025, 100000, 3000000));
	controller.packet_sender()->Process();
	REQUIRE_FALSE(router.sent.empty());
	REQUIRE(router.sent.front().probe_cluster_id == 55);
	REQUIRE(observer.rates.size() == 1);
}

TEST_CASE("ordinary GoogCC constraints preserve an estimate and enforce changed bounds", "[bandwidth-constraints]")
{
	webrtc::NetworkControllerConfig initial;
	initial.constraints = Constraints(1000, 100000, 3000000);
	initial.constraints.starting_rate = webrtc::DataRate::bps(600000);
	webrtc::GoogCcNetworkController controller(initial, webrtc::GoogCcConfig{});
	webrtc::ProcessInterval process;
	process.at_time = webrtc::Timestamp::ms(1000);
	controller.OnProcessInterval(process);

	// Obtain a lower estimate through real receiver feedback, then release that
	// receiver ceiling. Changing allocation/bounds must not reseed the estimate.
	webrtc::RemoteBitrateReport feedback;
	feedback.receive_time = webrtc::Timestamp::ms(1100);
	feedback.bandwidth    = webrtc::DataRate::bps(300000);
	controller.OnRemoteBitrateReport(feedback);
	process.at_time = feedback.receive_time;
	controller.OnProcessInterval(process);
	REQUIRE(controller.GetNetworkState(process.at_time).target_rate->target_rate.bps() == 300000);
	feedback.receive_time = webrtc::Timestamp::ms(1200);
	feedback.bandwidth    = webrtc::DataRate::bps(3000000);
	controller.OnRemoteBitrateReport(feedback);
	for (int64_t atMs = 1200; atMs <= 1450; atMs += 25)
	{
		controller.OnStreamsConfig(Streams(atMs, 100000, 3000000));
		auto update = controller.OnTargetRateConstraints(Constraints(atMs, 100000, 3000000));
		REQUIRE_FALSE(update.target_rate.has_value());
		REQUIRE(controller.GetNetworkState(webrtc::Timestamp::ms(atMs)).target_rate->target_rate.bps() == 300000);
	}

	// A raised minimum remains authoritative without supplying a new start.
	auto raised = controller.OnTargetRateConstraints(Constraints(1500, 400000, 3000000));
	REQUIRE(raised.target_rate.has_value());
	REQUIRE(raised.target_rate->target_rate.bps() == 400000);

	// Explicit reseeding is a distinct operation, and still works when requested.
	auto reseed = Constraints(1600, 100000, 3000000);
	reseed.starting_rate = webrtc::DataRate::bps(600000);
	auto restarted = controller.OnTargetRateConstraints(reseed);
	REQUIRE(restarted.target_rate.has_value());
	REQUIRE(restarted.target_rate->target_rate.bps() == 600000);

	// Maximum changes are applied by the normal estimator tick, without forcing
	// a starting rate to get the bound enforced.
	controller.OnTargetRateConstraints(Constraints(1700, 100000, 200000));
	process.at_time = webrtc::Timestamp::ms(1725);
	controller.OnProcessInterval(process);
	REQUIRE(controller.GetNetworkState(process.at_time).target_rate->target_rate.bps() == 200000);
}

TEST_CASE("real GoogCC allocation feedback is safe during synchronous observer reentry", "[bandwidth-constraints]")
{
	PacketRouter router;
	Observer observer;
	webrtc::GoogCcFactoryConfig config;
	config.feedback_only = true;
	webrtc::GoogCcNetworkControllerFactory factory(std::move(config));
	webrtc::BitrateConstraints initial;
	initial.start_bitrate_bps = 600000;
	webrtc::RtpTransportControllerSend controller(&router, nullptr, &factory, initial);
	int64_t minimum = 100000;
	const auto atMs = DepLibUV::GetTimeMs();
	observer.onRate = [&]()
	{
		controller.SetAllocatedSendBitrateLimits(minimum, 2550000, 3000000);
		controller.SetClientBitratePreferences(Constraints(atMs, minimum, 3000000));
	};
	controller.RegisterTargetTransferRateObserver(&observer);
	controller.OnNetworkAvailability(true);
	REQUIRE(observer.rates == std::vector<int64_t>{ 600000 });
	REQUIRE(observer.maximumDepth == 1);
	minimum = 800000;
	controller.SetAllocatedSendBitrateLimits(minimum, 2550000, 3000000);
	controller.SetClientBitratePreferences(Constraints(atMs, minimum, 3000000));
	REQUIRE(observer.rates == std::vector<int64_t>{ 600000, 800000 });
	REQUIRE(observer.maximumDepth == 1);
	controller.SetClientBitratePreferences(Constraints(atMs, minimum, 3000000));
	REQUIRE(observer.rates.size() == 2);
}

TEST_CASE("the client accepts a real target equal to its initial rate", "[bandwidth-constraints]")
{
	int64_t nowMs = 1000;
	mocks::MockShared shared([&]() { return nowMs * 1000; });
	ClientListener listener;
	RTC::TransportCongestionControlClient client(
	  &listener, &shared, RTC::BweType::TRANSPORT_CC, 100000, 600000, 3000000, 100000);
	client.TransportConnected();
	REQUIRE(client.GetBitrates().startBitrate == 600000);

	webrtc::TargetTransferRate target;
	target.target_rate = webrtc::DataRate::bps(200000);
	client.OnTargetTransferRate(target);
	REQUIRE(client.GetAvailableBitrate() == 200000);
	client.SetDesiredBitrate(2400000, true);
	REQUIRE(client.GetBitrates().startBitrate == 600000);
	REQUIRE(client.GetAvailableBitrate() == 200000);
	const auto notifications = listener.rates.size();
	nowMs = 2100;
	target.target_rate = webrtc::DataRate::bps(600000);
	client.OnTargetTransferRate(target);
	REQUIRE(client.GetAvailableBitrate() == 600000);
	REQUIRE(listener.rates.size() == notifications + 1);
	REQUIRE(listener.rates.back() == 600000);
	REQUIRE(client.GetBitrates().startBitrate == 600000);
}

TEST_CASE("ACK limited growth can prevent ALR and its periodic recovery probes", "[bandwidth-constraints]")
{
	for (const auto [initial, throughput] :
	     std::array<std::pair<int64_t, int64_t>, 2>{ { { 216000, 287000 }, { 281000, 334000 } } })
	{
		INFO("initial=" << initial << ", tracked sent/ACK=" << throughput);
		const auto current = ModelRecovery(false, initial, throughput);
		REQUIRE(current.estimate == throughput * 3 / 2 + 10000);
		REQUIRE(current.maximumEstimate == current.estimate);
		REQUIRE_FALSE(current.firstAlrAt.has_value());
		REQUIRE(current.periodicProbes == 0);

		// Diagnostic comparison only: this legacy switch removes the ACK cap.
		// Upstream WebRTC bug10542 later rejected that unbounded growth; this
		// test demonstrates the deadzone, not a production recommendation.
		const auto legacy = ModelRecovery(true, initial, throughput);
		REQUIRE(legacy.firstAlrAt.has_value());
		REQUIRE(legacy.firstProbeAt.has_value());
		REQUIRE(*legacy.firstProbeAt >= *legacy.firstAlrAt + 5000);
		REQUIRE(legacy.periodicProbes > 0);
		REQUIRE(legacy.estimate == legacy.alrEstimate);
		REQUIRE(legacy.estimate > current.estimate);
		REQUIRE(legacy.maximumEstimate < 3000000);
	}
}

TEST_CASE("ALR growth comparison retains overuse backoff and authoritative bounds", "[bandwidth-constraints]")
{
	for (const bool legacy : { false, true })
	{
		INFO("legacy ALR growth switch=" << legacy);
		RecoveryConfig config(legacy);
		webrtc::AimdRateControl aimd(&config, /*send_side*/ true);
		aimd.SetMinBitrate(webrtc::DataRate::bps(100000));
		aimd.SetStartBitrate(webrtc::DataRate::bps(600000));
		aimd.SetInApplicationLimitedRegion(true);
		webrtc::RateControlInput overuse(
		  webrtc::BandwidthUsage::kBwOverusing, webrtc::DataRate::bps(150000));
		const auto reduced = aimd.Update(&overuse, webrtc::Timestamp::ms(1000));
		REQUIRE(reduced.bps() < 150000);
		REQUIRE(reduced.bps() >= 100000);
		overuse.estimated_throughput = webrtc::DataRate::bps(40000);
		REQUIRE(aimd.Update(&overuse, webrtc::Timestamp::ms(2000)).bps() == 100000);

		webrtc::NetworkControllerConfig initial;
		initial.constraints = Constraints(1000, 100000, 3000000);
		initial.constraints.starting_rate = webrtc::DataRate::bps(600000);
		initial.key_value_config = &config;
		webrtc::GoogCcNetworkController controller(initial, webrtc::GoogCcConfig{});
		webrtc::ProcessInterval process;
		process.at_time = webrtc::Timestamp::ms(1000);
		controller.OnProcessInterval(process);
		webrtc::RemoteBitrateReport feedback;
		feedback.receive_time = webrtc::Timestamp::ms(1100);
		feedback.bandwidth = webrtc::DataRate::bps(8000000);
		controller.OnRemoteBitrateReport(feedback);
		process.at_time = feedback.receive_time;
		controller.OnProcessInterval(process);
		REQUIRE(controller.GetNetworkState(process.at_time).target_rate->target_rate.bps() == 3000000);
	}
}

TEST_CASE("unmet allocation retries native probes without reseeding the estimate", "[bandwidth-constraints]")
{
	RecoveryConfig config(false);
	webrtc::ProbeController prober(&config);
	prober.EnablePeriodicAlrProbing(true);
	InitializeDemandProbe(prober);
	REQUIRE(prober.Process(5999).empty());
	const auto first = prober.Process(6000);
	REQUIRE(first.size() == 1);
	REQUIRE(first.front().target_data_rate.bps() == 432286);
	REQUIRE(prober.Process(6500).empty());
	REQUIRE(prober.Process(7001).empty());
	REQUIRE(prober.Process(10999).empty());
	const auto retry = prober.Process(11000);
	REQUIRE(retry.size() == 1);
	REQUIRE(retry.front().target_data_rate == first.front().target_data_rate);
	REQUIRE(retry.front().id != first.front().id);

	// An unsuccessful probe has not raised the estimate. Only fresh estimator
	// feedback changes the next probe target, using the unchanged 2x scale.
	REQUIRE(prober.SetEstimatedBitrate(250000, 11001).empty());
	REQUIRE(prober.Process(12001).empty());
	const auto afterFeedback = prober.Process(16000);
	REQUIRE(afterFeedback.size() == 1);
	REQUIRE(afterFeedback.front().target_data_rate.bps() == 500000);
}

TEST_CASE("explicit demand probes escape starvation while retaining the ACK ceiling", "[bandwidth-constraints]")
{
	for (const auto [initial, throughput] :
	     std::array<std::pair<int64_t, int64_t>, 2>{ { { 216000, 287000 }, { 281000, 334000 } } })
	{
		INFO("initial=" << initial << ", tracked sent/ACK=" << throughput);
		const auto result = ModelRecovery(false, initial, throughput, 2400000);
		REQUIRE_FALSE(result.firstAlrAt.has_value());
		REQUIRE(result.firstProbeAt.has_value());
		REQUIRE(*result.firstProbeAt == 6000);
		REQUIRE(result.periodicProbes > 0);
		// These modeled probes receive no successful capacity feedback. Merely
		// scheduling them must not raise the estimate above its real ACK ceiling.
		REQUIRE(result.estimate == throughput * 3 / 2 + 10000);
	}
}

TEST_CASE("periodic demand probing respects demand lifecycle and network availability", "[bandwidth-constraints]")
{
	RecoveryConfig config(false);
	webrtc::ProbeController prober(&config);
	prober.EnablePeriodicAlrProbing(true);

	SECTION("zero video allocation")
	{
		InitializeDemandProbe(prober, 0);
		REQUIRE(prober.Process(6000).empty());
		REQUIRE(prober.Process(16000).empty());
	}
	SECTION("satisfied demand")
	{
		InitializeDemandProbe(prober, 216143);
		REQUIRE(prober.Process(6000).empty());
	}
	SECTION("maximum already reached")
	{
		InitializeDemandProbe(prober, 3000000, 3000000);
		REQUIRE(prober.Process(6000).empty());
	}
	SECTION("disabled periodic probing")
	{
		prober.EnablePeriodicAlrProbing(false);
		InitializeDemandProbe(prober);
		REQUIRE(prober.Process(6000).empty());
		prober.EnablePeriodicAlrProbing(true);
		REQUIRE(prober.Process(6000).size() == 1);
	}
	SECTION("no estimator result")
	{
		InitializeDemandProbe(prober);
		REQUIRE(prober.SetEstimatedBitrate(0, 2500).empty());
		REQUIRE(prober.Process(6000).empty());
	}
	SECTION("offline demand changes and recovery")
	{
		InitializeDemandProbe(prober);
		webrtc::NetworkAvailability network;
		network.at_time = webrtc::Timestamp::ms(3000);
		network.network_available = false;
		REQUIRE(prober.OnNetworkAvailability(network).empty());
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(2300000, 6000).empty());
		REQUIRE(prober.Process(6000).empty());
		network.at_time = webrtc::Timestamp::ms(7000);
		network.network_available = true;
		REQUIRE(prober.OnNetworkAvailability(network).empty());
		REQUIRE(prober.Process(7000).size() == 1);
	}
	SECTION("video stops while a recovery probe is pending")
	{
		InitializeDemandProbe(prober);
		REQUIRE(prober.Process(6000).size() == 1);
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(0, 6500).empty());
		REQUIRE(prober.Process(7001).empty());
		REQUIRE(prober.Process(11000).empty());
	}
}

TEST_CASE("allocation drift shares the periodic probe deadline", "[bandwidth-constraints]")
{
	RecoveryConfig config(false);
	webrtc::ProbeController prober(&config);
	prober.EnablePeriodicAlrProbing(true);
	InitializeDemandProbe(prober);
	REQUIRE(prober.OnMaxTotalAllocatedBitrate(2390000, 2100).empty());
	REQUIRE(prober.OnMaxTotalAllocatedBitrate(2380000, 4000).empty());
	REQUIRE(prober.Process(5999).empty());
	REQUIRE(prober.Process(6000).size() == 1);
	REQUIRE(prober.Process(7001).empty());
	REQUIRE(prober.OnMaxTotalAllocatedBitrate(2350000, 7100).empty());
	REQUIRE(prober.OnMaxTotalAllocatedBitrate(2400000, 7200).empty());
	REQUIRE(prober.Process(10999).empty());
	const auto due = prober.Process(11000);
	REQUIRE(due.size() == 1);
	REQUIRE(due.front().target_data_rate.bps() == 432286);

	SECTION("latest satisfied demand cancels the next retry")
	{
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(100000, 11200).empty());
		REQUIRE(prober.Process(12001).empty());
		REQUIRE(prober.Process(16000).empty());
	}
}

TEST_CASE("demand probe bounds preserve explicit caps and ordinary ALR cadence", "[bandwidth-constraints]")
{
	SECTION("demand has a five second minimum even with a shorter ALR interval")
	{
		RecoveryConfig config(false, "alr_interval:100ms");
		webrtc::ProbeController prober(&config);
		prober.EnablePeriodicAlrProbing(true);
		InitializeDemandProbe(prober, 2400000, 2000000);
		REQUIRE(prober.Process(5999).empty());
		const auto probes = prober.Process(6000);
		REQUIRE(probes.size() == 1);
		REQUIRE(probes.front().target_data_rate.bps() == 3000000);
		REQUIRE(prober.Process(6001).empty());
	}
	SECTION("a longer configured ALR interval also bounds demand probing")
	{
		RecoveryConfig config(false, "alr_interval:7s");
		webrtc::ProbeController prober(&config);
		prober.EnablePeriodicAlrProbing(true);
		InitializeDemandProbe(prober);
		REQUIRE(prober.Process(7999).empty());
		REQUIRE(prober.Process(8000).size() == 1);
	}
	SECTION("ordinary ALR retains its own configured cadence")
	{
		RecoveryConfig config(false, "alr_interval:100ms");
		webrtc::ProbeController prober(&config);
		prober.EnablePeriodicAlrProbing(true);
		InitializeDemandProbe(prober, 0);
		prober.SetAlrStartTimeMs(2001);
		REQUIRE(prober.Process(2100).empty());
		const auto probes = prober.Process(2101);
		REQUIRE(probes.size() == 1);
		REQUIRE(probes.front().target_data_rate.bps() == 432286);
	}
	SECTION("allocation changes without periodic mode keep existing immediate behavior")
	{
		RecoveryConfig config(false);
		webrtc::ProbeController prober(&config);
		InitializeDemandProbe(prober, 0);
		const auto probes = prober.OnMaxTotalAllocatedBitrate(2400000, 2100);
		REQUIRE_FALSE(probes.empty());
		for (const auto& probe : probes)
		{
			REQUIRE(probe.target_data_rate.bps() <= 3000000);
		}
	}
}

TEST_CASE("explicit demand does not accelerate a newly entered ALR deadline", "[bandwidth-constraints]")
{
	RecoveryConfig config(false);
	webrtc::ProbeController prober(&config);
	prober.EnablePeriodicAlrProbing(true);
	InitializeDemandProbe(prober);
	prober.SetAlrStartTimeMs(5900);
	REQUIRE(prober.Process(6000).empty());
	REQUIRE(prober.Process(10899).empty());
	const auto probes = prober.Process(10900);
	REQUIRE(probes.size() == 1);
	REQUIRE(probes.front().target_data_rate.bps() == 432286);
}

TEST_CASE("allocation and maximum changes share one application probe deadline", "[bandwidth-constraints]")
{
	RecoveryConfig config(false);
	webrtc::ProbeController prober(&config);
	prober.EnablePeriodicAlrProbing(true);
	REQUIRE_FALSE(prober.SetBitrates(100000, 500000, 800000, 1000).empty());
	REQUIRE(prober.OnMaxTotalAllocatedBitrate(600000, 1100).empty());
	REQUIRE(prober.Process(2001).empty());
	const auto first = prober.Process(6000);
	REQUIRE(first.size() == 1);
	REQUIRE(first.front().target_data_rate.bps() == 800000);

	SECTION("the worker's allocation then maximum update cannot create a second burst")
	{
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(900000, 6100).empty());
		REQUIRE(prober.SetBitrates(100000, -1, 1000000, 6100).empty());
		REQUIRE(prober.Process(10999).empty());
		const auto due = prober.Process(11000);
		REQUIRE(due.size() == 1);
		// The deferred update retained the new maximum: the old cap was 800k.
		REQUIRE(due.front().target_data_rate.bps() == 1000000);
	}
	SECTION("offline maximum changes retain demand without sending probes")
	{
		webrtc::NetworkAvailability network;
		network.at_time = webrtc::Timestamp::ms(6050);
		network.network_available = false;
		REQUIRE(prober.OnNetworkAvailability(network).empty());
		REQUIRE(prober.OnMaxTotalAllocatedBitrate(900000, 11000).empty());
		REQUIRE(prober.SetBitrates(100000, -1, 1000000, 11000).empty());
		REQUIRE(prober.Process(11000).empty());
		network.at_time = webrtc::Timestamp::ms(12000);
		network.network_available = true;
		REQUIRE(prober.OnNetworkAvailability(network).empty());
		const auto online = prober.Process(12000);
		REQUIRE(online.size() == 1);
		REQUIRE(online.front().target_data_rate.bps() == 1000000);
	}
	SECTION("maximum changes without periodic mode retain immediate behavior")
	{
		prober.EnablePeriodicAlrProbing(false);
		const auto immediate = prober.SetBitrates(100000, -1, 1000000, 6100);
		REQUIRE(immediate.size() == 1);
		REQUIRE(immediate.front().target_data_rate.bps() == 1000000);
	}
}
