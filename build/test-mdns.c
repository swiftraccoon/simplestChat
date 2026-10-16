// Disposable diagnostic for the exact macOS mDNS registration path used by
// WebKit's NetworkMDNSRegister.cpp. It never changes an OS or browser setting.
#include <arpa/inet.h>
#include <dispatch/dispatch.h>
#include <dns_sd.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <uuid/uuid.h>

static DNSServiceRef service;
static bool finished;
static bool callback_observed;

static void finish(const char *stage, DNSServiceErrorType error, bool timed_out)
{
    if (finished)
        return;
    finished = true;
    bool deallocated = service != NULL;
    if (service) {
        DNSServiceRefDeallocate(service);
        service = NULL;
    }
    printf("{\"stage\":\"%s\",\"error\":%d,\"callbackObserved\":%s,\"timedOut\":%s,\"deallocated\":%s}\n",
        stage, error, callback_observed ? "true" : "false",
        timed_out ? "true" : "false", deallocated ? "true" : "false");
    fflush(stdout);
    exit(callback_observed && !error && !timed_out ? EXIT_SUCCESS : EXIT_FAILURE);
}

static void registered(DNSServiceRef reference, DNSRecordRef record,
    DNSServiceFlags flags, DNSServiceErrorType error, void *context)
{
    (void)reference;
    (void)record;
    (void)flags;
    (void)context;
    callback_observed = true;
    finish("callback", error, false);
}

int main(int argc, char **argv)
{
    struct in_addr address;
    uuid_t identifier;
    if (argc != 3 || inet_pton(AF_INET, argv[1], &address) != 1
        || uuid_parse(argv[2], identifier))
        finish("arguments", 0, false);
    char canonical[37];
    char name[43];
    uuid_unparse_lower(identifier, canonical);
    snprintf(name, sizeof(name), "%s.local", canonical);

    DNSServiceErrorType error = DNSServiceCreateConnection(&service);
    if (error)
        finish("create_connection", error, false);
    error = DNSServiceSetDispatchQueue(service, dispatch_get_main_queue());
    if (error)
        finish("set_dispatch_queue", error, false);
    DNSRecordRef record = NULL;
    // Matches WebKit on current macOS: KnownUnique, interface 0, IPv4 A/IN,
    // four address bytes and TTL 0. Only a random disposable .local name exists
    // until the callback or deadline closes this process's service connection.
    error = DNSServiceRegisterRecord(service, &record,
        kDNSServiceFlagsKnownUnique, 0, name, kDNSServiceType_A,
        kDNSServiceClass_IN, sizeof(address), &address, 0, registered, NULL);
    if (error)
        finish("register_record", error, false);
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC),
        dispatch_get_main_queue(), ^{ finish("callback_timeout", 0, true); });
    dispatch_main();
}
