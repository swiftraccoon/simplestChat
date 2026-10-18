#include "DepLibUV.hpp"
#include "RTC/TransportCongestionControlClient.hpp"
#include "mocks/include/MockShared.hpp"
#include <catch2/catch_test_macros.hpp>
#include <libwebrtc/modules/congestion_controller/goog_cc/goog_cc_network_control.h>
#include <algorithm>
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
