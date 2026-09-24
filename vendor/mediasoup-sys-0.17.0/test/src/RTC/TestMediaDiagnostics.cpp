#include "RTC/MediaDiagnostics.hpp"
#include "RTC/RTP/RtpStreamRecv.hpp"
#include "mocks/include/MockShared.hpp"
#include <catch2/catch_test_macros.hpp>

namespace
{
	constexpr auto TransportId = "00112233-4455-6677-8899-aabbccddeeff";
	constexpr auto ProducerId = "11112233-4455-6677-8899-aabbccddeeff";

	class ManualTimer final : public TimerHandleInterface
	{
	public:
		ManualTimer(Listener *listener, uint64_t &now, std::function<void()> destroyed)
			: listener(listener), now(now), destroyed(std::move(destroyed))
		{
		}
		~ManualTimer() override
		{
			this->destroyed();
		}
		void Start(uint64_t timeout, uint64_t repeat = 0) override
		{
			this->timeout = timeout;
			this->repeat = repeat;
			this->due = this->now + timeout;
			this->active = true;
		}
		void Stop() override
		{
			this->active = false;
		}
		void Restart() override
		{
			Start(this->timeout, this->repeat);
		}
		void Restart(uint64_t timeout, uint64_t repeat = 0) override
		{
			Start(timeout, repeat);
		}
		uint64_t GetTimeout() const override
		{
			return this->timeout;
		}
		uint64_t GetRepeat() const override
		{
			return this->repeat;
		}
		bool IsActive() const override
		{
			return this->active;
		}
		void Advance()
		{
			if (this->active && this->now >= this->due)
			{
				this->active = this->repeat != 0;
				this->due = this->now + this->repeat;
				this->listener->OnTimer(this);
			}
		}

	private:
		Listener *listener;
		uint64_t &now;
		std::function<void()> destroyed;
		uint64_t timeout{0}, repeat{0}, due{0};
		bool active{false};
	};

	class TimedShared final : public mocks::MockShared
	{
	public:
		explicit TimedShared(uint64_t &now) : MockShared([&now]() { return now; }), now(now) {}
		TimerHandleInterface *CreateTimer(TimerHandleInterface::Listener *listener) override
		{
			REQUIRE(this->timer == nullptr);
			this->timer = new ManualTimer(listener, this->now,
										  [this]()
										  {
											  this->timer = nullptr;
											  ++this->destroyed;
										  });
			return this->timer;
		}
		void Advance(uint64_t now)
		{
			this->now = now;
			if (this->timer)
				this->timer->Advance();
		}
		ManualTimer *timer{nullptr};
		size_t destroyed{0};

	private:
		uint64_t &now;
	};

	class StreamListener final : public RTC::RTP::RtpStreamRecv::Listener
	{
	public:
		void OnRtpStreamScore(RTC::RTP::RtpStream *, uint8_t score, uint8_t previous) override
		{
			this->scores.emplace_back(previous, score);
		}
		void OnRtpStreamSendRtcpPacket(RTC::RTP::RtpStreamRecv *, RTC::RTCP::Packet *) override {}
		uint8_t OnRtpStreamNeedWorstRemoteFractionLost(RTC::RTP::RtpStreamRecv *) override
		{
			return 0;
		}
		std::vector<std::pair<uint8_t, uint8_t>> scores;
	};

	sockaddr_in Address(uint16_t port)
	{
		sockaddr_in address{};
		address.sin_family = AF_INET;
		address.sin_port = htons(port);
		address.sin_addr.s_addr = htonl(0x7f000001U);
		return address;
	}
} // namespace

TEST_CASE("media diagnostic identifiers cannot inject free text", "[media-diagnostics]")
{
	using RTC::MediaDiagnostics::Identifier;
	REQUIRE(std::string(Identifier(TransportId).Get()) == TransportId);
	for (const auto invalid : {"", "user@example.test", "00112233-4455-6677-8899-aabbccddeef\n",
							   "00112233-4455-6677-8899-AABBCCDDEEFF"})
	{
		REQUIRE(std::string(Identifier(invalid).Get()) == "unavailable");
	}
}

TEST_CASE("recent UDP tuple evidence owns addresses and expires or loses reused identity",
		  "[media-diagnostics]")
{
	using RTC::MediaDiagnostics::RecentTuples;
	RecentTuples cache;
	auto address = Address(1000);
	RTC::TransportTuple original(nullptr, reinterpret_cast<const sockaddr *>(&address));
	cache.Remove(&original, TransportId, 100);
	address.sin_port = htons(2000);
	REQUIRE_FALSE(cache.Find(&original, 101));
	auto prior = Address(1000);
	RTC::TransportTuple matching(nullptr, reinterpret_cast<const sockaddr *>(&prior));
	auto found = cache.Find(&matching, 101);
	REQUIRE(found.has_value());
	REQUIRE(std::string(found->transport.Get()) == TransportId);
	REQUIRE(found->removedMs == 100);
	cache.Add(&matching);
	REQUIRE_FALSE(cache.Find(&matching, 102));
	cache.Remove(&matching, ProducerId, 200);
	REQUIRE(cache.Find(&matching, 30'199));
	REQUIRE_FALSE(cache.Find(&matching, 30'200));
	cache.Remove(&matching, TransportId, 200);
	REQUIRE_FALSE(cache.Find(&matching, 199));
	RTC::TransportTuple tcp(static_cast<RTC::TcpConnection *>(nullptr));
	cache.Remove(&tcp, TransportId, 100);
	REQUIRE_FALSE(cache.Find(&tcp, 101));
	REQUIRE(cache.Size() == 0);
}

TEST_CASE("recent tuple evidence has a fixed capacity without retaining live routing state",
		  "[media-diagnostics]")
{
	RTC::MediaDiagnostics::RecentTuples cache;
	for (uint16_t port{1}; port <= 257; ++port)
	{
		auto address = Address(port);
		RTC::TransportTuple tuple(nullptr, reinterpret_cast<const sockaddr *>(&address));
		cache.Remove(&tuple, TransportId, port);
		REQUIRE(cache.Size() <= RTC::MediaDiagnostics::RecentTuples::Capacity);
	}
	auto oldest = Address(1);
	auto latest = Address(257);
	RTC::TransportTuple oldTuple(nullptr, reinterpret_cast<const sockaddr *>(&oldest));
	RTC::TransportTuple newTuple(nullptr, reinterpret_cast<const sockaddr *>(&latest));
	REQUIRE_FALSE(cache.Find(&oldTuple, 300));
	REQUIRE(cache.Find(&newTuple, 300));
}

TEST_CASE("unknown tuple counts flush after a finite burst and on server-owned destruction",
		  "[media-diagnostics]")
{
	using namespace RTC::MediaDiagnostics;
	uint64_t now{1000};
	TimedShared shared(now);
	std::vector<PacketDrops::Event> events;
	{
		PacketDrops drops(&shared, [&events](const auto &event) { events.push_back(event); });
		for (size_t i{0}; i < 100; ++i)
			drops.Record(PacketFamily::Rtp, std::nullopt, now);
		REQUIRE(events.size() == 1);
		REQUIRE_FALSE(events[0].summary);
		REQUIRE_FALSE(events[0].recentlyRemoved);
		REQUIRE(events[0].count == 1);
		shared.Advance(10'999);
		REQUIRE(events.size() == 1);
		shared.Advance(11'000);
		REQUIRE(events.size() == 2);
		REQUIRE(events[1].summary);
		REQUIRE(events[1].count == 99);
		REQUIRE(std::string(events[1].previousTransport.Get()) == "unavailable");
		const auto prior = RecentTuples::Match{Identifier(TransportId), 10'000};
		drops.Record(PacketFamily::Dtls, prior, now);
		drops.Record(PacketFamily::Dtls, prior, now);
		REQUIRE(events.size() == 3);
		REQUIRE(events[2].recentlyRemoved);
		REQUIRE(events[2].removedAgeMs == 1000);
		REQUIRE(std::string(events[2].previousTransport.Get()) == TransportId);
	}
	REQUIRE(events.size() == 4);
	REQUIRE(events[3].summary);
	REQUIRE(events[3].count == 1);
	REQUIRE(shared.timer == nullptr);
	REQUIRE(shared.destroyed == 1);
	shared.Advance(100'000);
	REQUIRE(events.size() == 4);
}

TEST_CASE("RTP inactivity evidence tracks media not padding and preserves pause and score behavior",
		  "[media-diagnostics]")
{
	uint64_t now{1000};
	TimedShared shared(now);
	StreamListener listener;
	RTC::RTP::RtpStream::Params params;
	params.ssrc = 5;
	params.encodingIdx = 2;
	params.clockRate = 48'000;
	params.mimeType.SetMimeType("audio/opus");
	alignas(4) uint8_t media[] = {0x80, 0x60, 0, 1, 0, 0, 0, 4, 0, 0, 0, 5, 0};
	alignas(4) uint8_t padding[] = {0xa0, 0x60, 0, 2, 0, 0, 0, 5, 0, 0, 0, 5, 1};
	std::unique_ptr<RTC::RTP::Packet> packet(RTC::RTP::Packet::Parse(media, sizeof(media), sizeof(media)));
	std::unique_ptr<RTC::RTP::Packet> pad(RTC::RTP::Packet::Parse(padding, sizeof(padding), sizeof(padding)));
	REQUIRE(packet);
	REQUIRE(pad);
	{
		RTC::RTP::RtpStreamRecv stream(&listener, &shared, params, 0, true, ProducerId, TransportId);
		REQUIRE_FALSE(stream.GetDiagnosticActivity().mediaMs);
		REQUIRE(stream.ReceivePacket(packet.get()));
		REQUIRE(stream.GetDiagnosticActivity().mediaMs == 1000);
		shared.Advance(2000);
		REQUIRE(stream.ReceivePacket(pad.get()));
		REQUIRE(stream.GetDiagnosticActivity().mediaMs == 1000);
		shared.Advance(2499);
		REQUIRE(stream.GetScore() == 10);
		shared.Advance(2500);
		REQUIRE(stream.GetScore() == 0);
		REQUIRE(listener.scores.size() == 1);
		shared.Advance(5000);
		REQUIRE(listener.scores.size() == 1);
		packet->SetSequenceNumber(3);
		REQUIRE(stream.ReceivePacket(packet.get()));
		REQUIRE(stream.GetScore() == 10);
		REQUIRE(stream.GetDiagnosticActivity().mediaMs == 5000);
		stream.Pause();
		REQUIRE(stream.GetDiagnosticActivity().paused);
		REQUIRE(stream.GetDiagnosticActivity().pauseMs == 5000);
		shared.Advance(10'000);
		REQUIRE(stream.GetScore() == 10);
		stream.Resume();
		REQUIRE_FALSE(stream.GetDiagnosticActivity().paused);
		REQUIRE(stream.GetDiagnosticActivity().resumeMs == 10'000);
		shared.Advance(11'500);
		REQUIRE(stream.GetScore() == 0);
	}
	REQUIRE(shared.timer == nullptr);
	REQUIRE(shared.destroyed == 1);
	params.useDtx = true;
	{
		RTC::RTP::RtpStreamRecv stream(&listener, &shared, params, 0, true, ProducerId, TransportId);
		shared.Advance(16'499);
		REQUIRE(stream.GetScore() == 10);
		shared.Advance(16'500);
		REQUIRE(stream.GetScore() == 0);
		REQUIRE_FALSE(stream.GetDiagnosticActivity().mediaMs);
	}
}
