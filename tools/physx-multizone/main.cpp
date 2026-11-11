#define NOMINMAX
#include <Windows.h>
#include "feature_harness.h"
#include "PsPvd.h"
#include "PxPvdClient.h"
#include "PxPvdDataStream.h"
#include "PxPvdCommStreamTypes.h"
#include "PxProfileZone.h"
#include "PxProfileEventBufferClient.h"
#include <array>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <utility>
#include <vector>

namespace {
using profile::PxProfileZone;
static_assert(PX_PHYSICS_VERSION_MAJOR == 3 && PX_PHYSICS_VERSION_MINOR == 4 && PX_PHYSICS_VERSION_BUGFIX == 2,
              "The producer requires PhysX 3.4.2");
constexpr std::uint64_t context_id = 0x435553540001ull;
constexpr const char* zone_name = "PvdStudio.MultiZone";
constexpr const char* shared_name = "PvdStudio.Multi.Shared";
constexpr const char* pending_name = "PvdStudio.Multi.Pending";

std::uint64_t counter() {
    LARGE_INTEGER value;
    if (!QueryPerformanceCounter(&value)) throw std::runtime_error("QPC failed");
    return static_cast<std::uint64_t>(value.QuadPart);
}

void check(pvdsdk::PvdError value) {
    if (value != pvdsdk::PvdErrorType::Success) throw std::runtime_error("SDK PVD stream rejected an operation");
}

struct Call {
    std::size_t sequence;
    unsigned zone, frame;
    bool start;
    std::uint16_t event;
    std::uint64_t lower, upper;
    std::size_t start_sequence;
};
struct Flush {
    unsigned zone, frame;
    std::vector<std::uint8_t> bytes;
};

struct Zones : pvdsdk::PvdClient {
    struct Client : profile::PxProfileZoneClient {
        Zones& owner;
        unsigned index;
        PxProfileZone& zone;
        bool attached = false;

        Client(Zones& owner, unsigned index, PxAllocatorCallback& allocator) :
            owner(owner), index(index), zone(make(allocator)) {
            owner.stream->addProfileZone(&zone, zone.getName());
            check(owner.stream->createInstance(&zone));
            zone.addClient(*this);
            attached = true;
            const auto names = zone.getProfileNames();
            for (std::uint32_t i = 0; i < names.eventCount; ++i) handleEventAdded(names.events[i]);
        }
        static PxProfileZone& make(PxAllocatorCallback& allocator) {
            const profile::PxProfileEventName names[] = {
                {shared_name, profile::PxProfileEventId(17, true)},
                {pending_name, profile::PxProfileEventId(18, true)}
            };
            return PxProfileZone::createProfileZone(&allocator, zone_name, profile::PxProfileNames(2, names), 65536);
        }
        ~Client() {
            if (attached) zone.removeClient(*this);
            zone.release();
        }
        void handleEventAdded(const profile::PxProfileEventName& name) override {
            owner.stream->addProfileZoneEvent(&zone, name.name, name.eventId.eventId, name.eventId.compileTimeEnabled);
        }
        void handleBufferFlush(const std::uint8_t* bytes, std::uint32_t length) override {
            if ((!bytes && length) || length > 65536 || owner.raw_bytes + length > 16 * 1024 * 1024)
                throw std::runtime_error("Unexpected SDK profile buffer volume");
            owner.raw_bytes += length;
            std::vector<std::uint8_t> captured;
            if (length) captured.assign(bytes, bytes + length);
            owner.flushes.push_back({index, owner.frame, std::move(captured)});
            check(owner.stream->setPropertyValue(&zone, "events", bytes, length));
        }
        void handleClientRemoved() override {
            owner.stream->destroyInstance(&zone);
            attached = false;
        }
    };

    pvdsdk::PsPvd& pvd;
    pvdsdk::PvdDataStream* stream;
    std::array<std::unique_ptr<Client>, 2> clients;
    std::vector<Call> calls;
    std::vector<Flush> flushes;
    unsigned frame = 0;
    std::size_t raw_bytes = 0;
    std::uint32_t thread = GetCurrentThreadId();
    bool connected = false;

    Zones(PxPvd& pvd, PxAllocatorCallback& allocator) :
        pvd(static_cast<pvdsdk::PsPvd&>(pvd)),
        stream(feature_require(pvdsdk::PvdDataStream::create(&pvd), "multi-zone stream")) {
        clients[0].reset(new Client(*this, 0, allocator));
        clients[1].reset(new Client(*this, 1, allocator));
        this->pvd.addClient(this);
    }
    ~Zones() {
        pvd.removeClient(this);
        clients[1].reset();
        clients[0].reset();
        stream->release();
    }
    pvdsdk::PvdDataStream* getDataStream() override { return stream; }
    pvdsdk::PvdUserRenderer* getUserRender() override { return nullptr; }
    bool isConnected() const override { return connected; }
    void onPvdConnected() override { connected = true; }
    void onPvdDisconnected() override { connected = false; }
    void flush() override {
        for (auto& client : clients) client->zone.flushProfileEvents();
    }
    std::size_t start(unsigned zone, std::uint16_t event) {
        const auto index = calls.size();
        const auto lower = counter();
        clients.at(zone)->zone.startEvent(event, context_id, thread);
        const auto upper = counter();
        calls.push_back({index, zone, frame, true, event, lower, upper, index});
        return index;
    }
    void stop(unsigned zone, std::uint16_t event, std::size_t start) {
        const auto& first = calls.at(start);
        if (!first.start || first.zone != zone || first.event != event)
            throw std::runtime_error("Invalid explicit producer span pair");
        const auto lower = counter();
        clients.at(zone)->zone.stopEvent(event, context_id, thread);
        const auto upper = counter();
        calls.push_back({calls.size(), zone, frame, false, event, lower, upper, start});
    }
    void write(const char* path, std::uint64_t scene) const {
        std::ofstream out(path, std::ios::binary);
        if (!out) throw std::runtime_error("Cannot open multi-zone truth output");
        out << "{\"sdk\":\"3.4.2\",\"scene\":" << scene << ",\"frames\":8,\"context\":" << context_id
            << ",\"thread\":" << thread << ",\"zones\":[";
        for (unsigned i = 0; i < 2; ++i) {
            if (i) out << ',';
            out << "{\"label\":" << i << ",\"id\":" << reinterpret_cast<std::uintptr_t>(&clients[i]->zone)
                << ",\"name\":\"" << zone_name << "\"}";
        }
        out << "],\"calls\":[";
        for (std::size_t i = 0; i < calls.size(); ++i) {
            const auto& call = calls[i];
            if (i) out << ',';
            out << "{\"sequence\":" << call.sequence << ",\"zone\":" << call.zone << ",\"frame\":" << call.frame
                << ",\"kind\":\"" << (call.start ? "start" : "stop") << "\",\"event\":" << call.event
                << ",\"lower\":" << call.lower << ",\"upper\":" << call.upper
                << ",\"start_sequence\":" << call.start_sequence << '}';
        }
        out << "],\"flushes\":[";
        for (std::size_t i = 0; i < flushes.size(); ++i) {
            const auto& flush = flushes[i];
            if (i) out << ',';
            out << "{\"zone\":" << flush.zone << ",\"frame\":" << flush.frame << ",\"hex\":\"";
            for (auto byte : flush.bytes) out << std::hex << std::setw(2) << std::setfill('0') << unsigned(byte);
            out << std::dec << "\"}";
        }
        out << "]}\n";
        out.close();
        if (!out) throw std::runtime_error("Cannot flush multi-zone truth output");
    }
};
}

int main(int argc, char** argv) {
    if (argc != 3) { std::cerr << "usage: pvd-physx-multizone capture.pvdstream truth.json\n"; return 2; }
    try {
        for (const auto* path : {argv[1], argv[2]})
            if (GetFileAttributesA(path) != INVALID_FILE_ATTRIBUTES) throw std::runtime_error("Output already exists");
        FeatureContext context(argv[1], PxPvdInstrumentationFlag::eDEBUG | PxPvdInstrumentationFlag::ePROFILE);
        Zones zones(*context.pvd, context.allocator);
        std::size_t a0 = 0, b0 = 0, inner = 0, ap = 0, bp = 0, ac = 0, al = 0, bl = 0;
        for (zones.frame = 0; zones.frame < 8; ++zones.frame) {
            switch (zones.frame) {
                case 0: a0 = zones.start(0, 17); b0 = zones.start(1, 17); break;
                case 1: {
                    zones.stop(0, 17, a0);
                    auto immediate = zones.start(0, 17);
                    zones.stop(0, 17, immediate);
                    break;
                }
                case 2: inner = zones.start(1, 17); ap = zones.start(0, 18); bp = zones.start(1, 18); break;
                case 3: zones.stop(1, 17, inner); ac = zones.start(0, 17); zones.stop(0, 18, ap); break;
                case 4: zones.stop(0, 17, ac); zones.stop(1, 17, b0); al = zones.start(0, 17); bl = zones.start(1, 17); break;
                case 5: zones.stop(1, 17, bl); zones.stop(1, 18, bp); zones.start(1, 18); break;
                case 6: zones.stop(0, 17, al); break;
            }
            context.step();
            context.flush_and_require_connected();
        }
        zones.frame = 7;
        zones.flush();
        context.flush_and_require_connected();
        zones.write(argv[2], reinterpret_cast<std::uintptr_t>(context.scene));
        std::cout << "8 SDK frames, 2 same-name zones, " << zones.calls.size()
                  << " direct calls, 9 completed spans, 1 pending start\n";
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
