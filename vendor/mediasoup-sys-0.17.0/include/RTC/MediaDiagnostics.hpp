#ifndef MS_RTC_MEDIA_DIAGNOSTICS_HPP
#define MS_RTC_MEDIA_DIAGNOSTICS_HPP

#include "RTC/MediaDiagnosticId.hpp"
#include "RTC/TransportTuple.hpp"
#include "SharedInterface.hpp"
#include "handles/TimerHandleInterface.hpp"
#include <ankerl/unordered_dense.h>
#include <algorithm>
#include <array>
#include <functional>
#include <limits>
#include <memory>
#include <optional>
#include <string_view>

namespace RTC::MediaDiagnostics
{

	class RecentTuples
	{
	public:
		static constexpr size_t Capacity{256};
		static constexpr uint64_t LifetimeMs{30'000};

		struct Match
		{
			Identifier transport;
			uint64_t removedMs;
		};

		void Remove(const RTC::TransportTuple *tuple, std::string_view transport, uint64_t nowMs)
		{
			// A TCP key is a recyclable connection pointer, not a durable identity.
			// Do not attribute a later connection to it. UDP sockets live with this
			// server and each retained tuple owns its remote-address bytes.
			if (tuple->GetProtocol() != RTC::TransportTuple::Protocol::UDP)
			{
				return;
			}
			this->entries.erase(tuple->GetTupleKey());
			if (this->entries.size() == Capacity)
			{
				auto oldest = std::min_element(this->entries.begin(), this->entries.end(),
											   [](const auto &a, const auto &b)
											   { return a.second->removedMs < b.second->removedMs; });
				this->entries.erase(oldest);
			}
			auto entry = std::make_unique<Entry>(tuple, transport, nowMs);
			const auto key = entry->tuple.GetTupleKey();
			this->entries.emplace(key, std::move(entry));
		}

		void Add(const RTC::TransportTuple *tuple)
		{
			this->entries.erase(tuple->GetTupleKey());
		}

		std::optional<Match> Find(const RTC::TransportTuple *tuple, uint64_t nowMs)
		{
			auto it = this->entries.find(tuple->GetTupleKey());
			if (it == this->entries.end())
			{
				return std::nullopt;
			}
			if (nowMs < it->second->removedMs || nowMs - it->second->removedMs >= LifetimeMs)
			{
				this->entries.erase(it);
				return std::nullopt;
			}
			return Match{it->second->transport, it->second->removedMs};
		}

		size_t Size() const
		{
			return this->entries.size();
		}

	private:
		struct Entry
		{
			Entry(const RTC::TransportTuple *tuple, std::string_view transport, uint64_t removedMs)
				: tuple(tuple), transport(transport), removedMs(removedMs)
			{
			}
			RTC::TransportTuple tuple;
			Identifier transport;
			uint64_t removedMs;
		};
		ankerl::unordered_dense::map<RTC::TransportTuple::TupleKey, std::unique_ptr<Entry>,
									 RTC::TransportTuple::TupleKeyHash>
			entries;
	};

	enum class PacketFamily : uint8_t
	{
		Rtp,
		Rtcp,
		Dtls,
		Other
	};

	inline const char *PacketFamilyName(PacketFamily family)
	{
		switch (family)
		{
		case PacketFamily::Rtp:
			return "rtp";
		case PacketFamily::Rtcp:
			return "rtcp";
		case PacketFamily::Dtls:
			return "dtls";
		case PacketFamily::Other:
			return "other";
		}
		return "other";
	}

	// One exemplar per fixed family/history class per window. A one-shot timer
	// flushes every coalesced count even when the input stops; destruction flushes
	// pending counts before destroying that timer. Never retains packet bytes.
	class PacketDrops : public TimerHandleInterface::Listener
	{
	public:
		static constexpr uint64_t WindowMs{10'000};
		struct Event
		{
			PacketFamily family;
			bool recentlyRemoved;
			bool summary;
			uint64_t count;
			Identifier previousTransport;
			uint64_t removedAgeMs;
		};

		PacketDrops(SharedInterface *shared, std::function<void(const Event &)> emit)
			: emit(std::move(emit)), timer(shared->CreateTimer(this))
		{
		}
		PacketDrops(const PacketDrops &) = delete;
		PacketDrops &operator=(const PacketDrops &) = delete;

		~PacketDrops() override
		{
			this->timer->Stop();
			Flush();
			delete this->timer;
		}

		void Record(PacketFamily family, const std::optional<RecentTuples::Match> &previous, uint64_t nowMs)
		{
			if (!this->windowActive)
			{
				this->windowActive = true;
				this->timer->Start(WindowMs);
			}
			const size_t index = static_cast<size_t>(family) * 2 + previous.has_value();
			auto &count = this->counts[index];
			if (count == 0)
			{
				this->emit(Event{family, previous.has_value(), false, 1,
								 previous ? previous->transport : Identifier{},
								 previous ? nowMs - previous->removedMs : 0});
			}
			if (count < std::numeric_limits<uint64_t>::max())
			{
				++count;
			}
		}

	private:
		void OnTimer(TimerHandleInterface *timer) override
		{
			if (timer == this->timer)
			{
				Flush();
			}
		}

		void Flush()
		{
			for (size_t i{0}; i < this->counts.size(); ++i)
			{
				if (this->counts[i] > 1)
				{
					this->emit(Event{static_cast<PacketFamily>(i / 2), i % 2 != 0, true, this->counts[i] - 1,
									 Identifier{}, 0});
				}
				this->counts[i] = 0;
			}
			this->windowActive = false;
		}

		std::function<void(const Event &)> emit;
		TimerHandleInterface *timer;
		std::array<uint64_t, 8> counts{};
		bool windowActive{false};
	};
} // namespace RTC::MediaDiagnostics

#endif
