#include "mocks/include/MockShared.hpp"
#include "RTC/DtlsTransport.hpp"
#include <openssl/err.h>
#include <openssl/sslerr.h>
#include <catch2/catch_test_macros.hpp>
#include <cerrno>
#include <deque>
#include <memory>
#include <vector>

namespace RTC
{
	// Only the MS_TEST build grants access. Faults are injected into owned SSL
	// instances; no packets, sockets, external peers or application credentials
	// are involved. The handshake/close tests use real OpenSSL DTLS records.
	class DtlsTransportTestAccess
	{
	public:
		static const X509* Certificate()
		{
			return DtlsTransport::certificate;
		}

		static bool HasResetReceiveState(const DtlsTransport& transport)
		{
			return !transport.timer->IsActive() && !transport.localRole.has_value() &&
			       !transport.handshakeDone && !transport.handshakeDoneNow &&
			       SSL_get_shutdown(transport.ssl) == 0 && SSL_is_init_finished(transport.ssl) == 0;
		}

		static bool FailSsl(DtlsTransport& transport, bool syscall, bool receivedShutdown)
		{
			ERR_clear_error();
			// OpenSSL 4 records the result of SSL I/O on the SSL object. Raising
			// an unrelated thread-local queue entry no longer makes I/O fail.
			int result;
			if (syscall)
			{
				BIO_set_callback_ex(transport.sslBioFromNetwork, FailBioRead);
				uint8_t buffer;
				result = SSL_read(transport.ssl, &buffer, sizeof(buffer));
				BIO_set_callback_ex(transport.sslBioFromNetwork, nullptr);
			}
			else
			{
				result = SSL_read(transport.ssl, nullptr, -1);
			}
			REQUIRE(result < 0);
			REQUIRE(SSL_get_error(transport.ssl, result) == (syscall ? SSL_ERROR_SYSCALL : SSL_ERROR_SSL));
			if (receivedShutdown)
			{
				SSL_set_shutdown(transport.ssl, SSL_RECEIVED_SHUTDOWN);
			}
			return transport.CheckStatus(result);
		}

		static void UseLongHandshakeTimeout(DtlsTransport& transport)
		{
			DTLS_set_timer_cb(transport.ssl, [](SSL*, unsigned int) { return 31000000u; });
		}

		static bool SetSrtpProfile(DtlsTransport& transport, const char* profile)
		{
			return SSL_set_tlsext_use_srtp(transport.ssl, profile) == 0;
		}

	private:
		static long FailBioRead(
		  BIO* bio, int operation, const char*, size_t, int command, long, int result, size_t*)
		{
			if (operation == BIO_CB_READ)
			{
				BIO_clear_retry_flags(bio);
				errno = EIO;
				ERR_raise(ERR_LIB_SYS, EIO);
				return -1;
			}
			// An I/O failure is neither a retry nor orderly EOF on this owned BIO.
			if (operation == BIO_CB_CTRL && command == BIO_CTRL_EOF)
			{
				return 0;
			}
			return (operation & BIO_CB_RETURN) != 0 ? result : 1;
		}
	};
} // namespace RTC

namespace
{
	using Dtls   = RTC::DtlsTransport;
	using Reason = Dtls::CloseReason;
	using State  = Dtls::DtlsState;
	using Access = RTC::DtlsTransportTestAccess;

	struct DtlsClass
	{
		DtlsClass()
		{
			Dtls::ClassInit();
		}
		~DtlsClass()
		{
			Dtls::ClassDestroy();
		}
	};

	class Listener : public Dtls::Listener
	{
	public:
		std::deque<std::vector<uint8_t>> outgoing;
		std::vector<Reason> closed;
		std::vector<Reason> failed;
		size_t connected{ 0 };
		size_t applicationData{ 0 };

		void OnDtlsTransportConnecting(const Dtls*) override
		{
		}
		void OnDtlsTransportConnected(
		  const Dtls*, RTC::SrtpSession::CryptoSuite, uint8_t*, size_t, uint8_t*, size_t, std::string&) override
		{
			++this->connected;
		}
		void OnDtlsTransportFailed(const Dtls* transport) override
		{
			this->failed.push_back(transport->GetCloseReason());
		}
		void OnDtlsTransportClosed(const Dtls* transport) override
		{
			this->closed.push_back(transport->GetCloseReason());
		}
		void OnDtlsTransportSendData(const Dtls*, const uint8_t* data, size_t len) override
		{
			this->outgoing.emplace_back(data, data + len);
		}
		void OnDtlsTransportApplicationDataReceived(const Dtls*, const uint8_t*, size_t, int64_t) override
		{
			++this->applicationData;
		}
	};

	class Pair
	{
	public:
		mocks::MockShared clientShared{ []() { return int64_t{ 0 }; } };
		mocks::MockShared serverShared{ []() { return int64_t{ 0 }; } };
		Listener clientEvents;
		Listener serverEvents;
		std::unique_ptr<Dtls> client{ std::make_unique<Dtls>(&this->clientEvents, &this->clientShared) };
		std::unique_ptr<Dtls> server{ std::make_unique<Dtls>(&this->serverEvents, &this->serverShared) };

		void Start(bool verifyServerPeer = true, bool wrongServerFingerprint = false)
		{
			// ClassInit appends newly generated fingerprints. The last fingerprint
			// belongs to this fixture's certificate even when another test ran first.
			auto fingerprint = Dtls::GetLocalFingerprints().back();
			REQUIRE(this->client->SetRemoteFingerprint(fingerprint));
			if (verifyServerPeer)
			{
				if (wrongServerFingerprint)
				{
					fingerprint.value[0] = fingerprint.value[0] == '0' ? '1' : '0';
				}
				REQUIRE(this->server->SetRemoteFingerprint(fingerprint));
			}
			this->server->Run(Dtls::Role::SERVER);
			this->client->Run(Dtls::Role::CLIENT);
			Pump();
		}

		void Connect()
		{
			Start();
			REQUIRE(this->client->GetState() == State::CONNECTED);
			REQUIRE(this->server->GetState() == State::CONNECTED);
		}

		void Pump()
		{
			// Deliver from a queue instead of re-entering OpenSSL from its write
			// callback. A broken handshake must fail within a fixed amount of work.
			for (size_t step{ 0 }; step < 256; ++step)
			{
				if (this->clientEvents.outgoing.empty() && this->serverEvents.outgoing.empty())
				{
					return;
				}
				Deliver(this->clientEvents, this->server.get());
				Deliver(this->serverEvents, this->client.get());
			}
			FAIL("owned DTLS pair exceeded its packet budget");
		}

	private:
		static void Deliver(Listener& sender, Dtls* receiver)
		{
			if (sender.outgoing.empty())
			{
				return;
			}
			auto data = std::move(sender.outgoing.front());
			sender.outgoing.pop_front();
			if (receiver && (receiver->GetState() == State::CONNECTING || receiver->GetState() == State::CONNECTED))
			{
				receiver->ProcessDtlsData(data.data(), data.size(), 0);
			}
		}
	};
} // namespace

TEST_CASE("DTLS generated certificate retains its subject and self signature", "[dtls]")
{
	DtlsClass dtlsClass;
	auto* certificate = Access::Certificate();
	REQUIRE(certificate != nullptr);
	const auto* subject = X509_get_subject_name(certificate);
	const auto* issuer  = X509_get_issuer_name(certificate);
	REQUIRE(subject != nullptr);
	REQUIRE(issuer != nullptr);
	CHECK(X509_NAME_cmp(subject, issuer) == 0);
	const auto commonNameIndex   = X509_NAME_get_index_by_NID(subject, NID_commonName, -1);
	const auto organizationIndex = X509_NAME_get_index_by_NID(subject, NID_organizationName, -1);
	REQUIRE(commonNameIndex >= 0);
	REQUIRE(organizationIndex >= 0);
	const auto* commonName = X509_NAME_ENTRY_get_data(X509_NAME_get_entry(subject, commonNameIndex));
	const auto* organization = X509_NAME_ENTRY_get_data(X509_NAME_get_entry(subject, organizationIndex));
	REQUIRE(commonName != nullptr);
	REQUIRE(organization != nullptr);
	CHECK(ASN1_STRING_cmp(commonName, organization) == 0);
	REQUIRE(ASN1_STRING_length(commonName) > 0);
	const std::string name(
	  reinterpret_cast<const char*>(ASN1_STRING_get0_data(commonName)),
	  static_cast<size_t>(ASN1_STRING_length(commonName)));
	CHECK(name.find("mediasoup") == 0);
	std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> publicKey(
	  X509_get_pubkey(certificate), EVP_PKEY_free);
	REQUIRE(publicKey != nullptr);
	CHECK(X509_verify(certificate, publicKey.get()) == 1);
}

TEST_CASE("DTLS authenticated peer close retains its reason after reset", "[dtls][dtls-close]")
{
	DtlsClass dtlsClass;
	Pair pair;
	pair.Connect();
	const uint8_t data[]{ 1, 2, 3 };
	REQUIRE(pair.client->SendApplicationData(data, sizeof(data)));
	pair.Pump();
	REQUIRE(pair.serverEvents.applicationData == 1);

	// The real peer destructor sends an encrypted close_notify.
	pair.client.reset();
	pair.Pump();
	CHECK(pair.server->GetState() == State::CLOSED);
	CHECK(pair.server->GetCloseReason() == Reason::PEER_CLOSE_NOTIFY);
	CHECK(pair.serverEvents.closed == std::vector<Reason>{ Reason::PEER_CLOSE_NOTIFY });
	CHECK(pair.serverEvents.failed.empty());
	CHECK(Access::HasResetReceiveState(*pair.server));

	// A fresh run clears the retained reason immediately. Application reconnects
	// allocate a new WebRTC transport; they do not reuse a terminal SSL session.
	pair.server->Run(Dtls::Role::SERVER);
	CHECK(pair.server->GetCloseReason() == Reason::NONE);
	CHECK(pair.server->GetState() == State::CONNECTING);
}

TEST_CASE("DTLS fatal and syscall errors cannot become orderly closes", "[dtls]")
{
	DtlsClass dtlsClass;
	for (const bool syscall : { false, true })
	{
		for (const bool shutdownFlag : { false, true })
		{
			Pair pair;
			pair.Connect();
			const auto expected = syscall ? Reason::SYSCALL_ERROR : Reason::SSL_ERROR;
			CHECK_FALSE(Access::FailSsl(*pair.server, syscall, shutdownFlag));
			// Preserve the existing wire state; the retained reason is the distinction.
			CHECK(pair.server->GetState() == State::CLOSED);
			CHECK(pair.server->GetCloseReason() == expected);
			CHECK(pair.serverEvents.closed == std::vector<Reason>{ expected });
			CHECK(Access::HasResetReceiveState(*pair.server));
			const uint8_t data[]{ 1, 2, 3 };
			CHECK_FALSE(pair.server->SendApplicationData(data, sizeof(data)));
		}
	}
}

TEST_CASE("DTLS close before fingerprint verification remains a failure", "[dtls]")
{
	DtlsClass dtlsClass;
	Pair pair;
	pair.Start(/*verifyServerPeer*/ false);
	REQUIRE(pair.client->GetState() == State::CONNECTED);
	REQUIRE(pair.server->GetState() == State::CONNECTING);
	pair.client.reset();
	pair.Pump();
	CHECK(pair.server->GetState() == State::FAILED);
	CHECK(pair.serverEvents.failed == std::vector<Reason>{ Reason::PEER_CLOSE_BEFORE_CONNECTED });
	CHECK(pair.serverEvents.closed.empty());
	CHECK(Access::HasResetReceiveState(*pair.server));
}

TEST_CASE("DTLS fingerprint failure survives SSL reset", "[dtls]")
{
	DtlsClass dtlsClass;
	Pair pair;
	pair.Start(/*verifyServerPeer*/ true, /*wrongServerFingerprint*/ true);
	CHECK(pair.server->GetState() == State::FAILED);
	CHECK(pair.serverEvents.failed == std::vector<Reason>{ Reason::FINGERPRINT_VALIDATION_FAILED });
	CHECK(pair.serverEvents.connected == 0);
	CHECK(Access::HasResetReceiveState(*pair.server));
}

TEST_CASE("DTLS incompatible SRTP profiles remain a negotiation failure", "[dtls]")
{
	DtlsClass dtlsClass;
	Pair pair;
	REQUIRE(Access::SetSrtpProfile(*pair.client, "SRTP_AES128_CM_SHA1_80"));
	REQUIRE(Access::SetSrtpProfile(*pair.server, "SRTP_AEAD_AES_256_GCM"));
	pair.Start();
	CHECK(pair.server->GetState() == State::FAILED);
	CHECK(pair.serverEvents.failed == std::vector<Reason>{ Reason::SRTP_NEGOTIATION_FAILED });
	CHECK(pair.serverEvents.connected == 0);
	CHECK(Access::HasResetReceiveState(*pair.server));
}

TEST_CASE("DTLS excessive handshake deadline retains timeout reason", "[dtls]")
{
	DtlsClass dtlsClass;
	Pair pair;
	Access::UseLongHandshakeTimeout(*pair.client);
	pair.client->Run(Dtls::Role::CLIENT);
	CHECK(pair.client->GetState() == State::FAILED);
	CHECK(pair.clientEvents.failed == std::vector<Reason>{ Reason::HANDSHAKE_TIMEOUT });
	CHECK(Access::HasResetReceiveState(*pair.client));
}
