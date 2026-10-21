#define NOMINMAX
#include <Windows.h>
#include "feature_harness.h"
#include <cstdint>
#include <exception>
#include <fstream>
#include <iostream>
#include <map>
#include <mutex>
#include <thread>
#include <tuple>
#include <vector>

static std::uint64_t counter() {
    LARGE_INTEGER value;
    if (!QueryPerformanceCounter(&value)) throw std::runtime_error("QPC failed");
    return static_cast<std::uint64_t>(value.QuadPart);
}

struct Call {
    std::size_t sequence;
    bool start;
    std::string event;
    std::uint64_t context;
    std::uint32_t thread;
    unsigned frame;
    std::uint64_t lower, upper;
};

struct Probe {
    using Key = std::tuple<std::string, std::uint64_t, std::uint32_t>;
    struct Start { Call call; void* token; };
    struct Span { Call start, end; };
    PxPvd& pvd;
    std::mutex mutex;
    std::vector<Call> calls;
    std::vector<Span> spans;
    std::map<Key, std::vector<Start>> pending;

    explicit Probe(PxPvd& client) : pvd(client) {}

    void start(const char* event, std::uint64_t context, unsigned frame) {
        std::lock_guard<std::mutex> guard(mutex);
        const auto thread = GetCurrentThreadId();
        const auto lower = counter();
        auto* token = pvd.zoneStart(event, false, context);
        const auto upper = counter();
        Call call{calls.size(), true, event, context, thread, frame, lower, upper};
        calls.push_back(call);
        pending[{event, context, thread}].push_back({call, token});
    }

    void stop(const char* event, std::uint64_t context, unsigned frame) {
        std::lock_guard<std::mutex> guard(mutex);
        const auto thread = GetCurrentThreadId();
        auto& starts = pending[{event, context, thread}];
        if (starts.empty()) throw std::runtime_error("Unmatched producer profile stop");
        const auto start = starts.back();
        const auto lower = counter();
        pvd.zoneEnd(start.token, event, false, context);
        const auto upper = counter();
        Call call{calls.size(), false, event, context, thread, frame, lower, upper};
        calls.push_back(call);
        spans.push_back({start.call, call});
        starts.pop_back();
    }
};

static void write_call(std::ostream& out, const Call& call) {
    out << "{\"sequence\":" << call.sequence << ",\"kind\":\"" << (call.start ? "start" : "stop")
        << "\",\"event\":\"" << call.event << "\",\"context\":" << call.context
        << ",\"thread\":" << call.thread << ",\"frame\":" << call.frame
        << ",\"lower\":" << call.lower << ",\"upper\":" << call.upper << '}';
}

int main(int argc, char** argv) {
    if (argc != 3) { std::cerr << "usage: pvd-profile-events capture.pvdstream truth.json\n"; return 2; }
    try {
        FeatureContext context(argv[1]);
        auto* ground = feature_require(PxCreatePlane(*context.physics, PxPlane(0, 1, 0, 0), *context.material), "ground");
        auto* box = feature_require(PxCreateDynamic(*context.physics, PxTransform(PxVec3(0, 2, 0)), PxBoxGeometry(PxVec3(0.5f)), *context.material, 1.0f), "box");
        context.scene->addActor(*ground);
        context.scene->addActor(*box);
        Probe probe(*context.pvd);
        constexpr std::uint64_t context_a = 0x435553540001ull;
        constexpr std::uint64_t context_b = 0x435553540002ull;
        const auto scene_id = reinterpret_cast<std::uintptr_t>(context.scene);
        if (scene_id == context_a || scene_id == context_b) throw std::runtime_error("Profile context unexpectedly aliases Scene");
        const auto main_thread = GetCurrentThreadId();
        std::uint32_t worker_thread = 0;
        const char* sample = "PvdStudio.Profile.Sample";
        const char* second = "PvdStudio.Profile.Second";
        const char* cross_frame = "PvdStudio.Profile.CrossFrame";
        const char* pending = "PvdStudio.Profile.Pending";
        std::vector<std::pair<std::uint64_t, std::uint64_t>> steps;
        for (unsigned frame = 0; frame < 8; ++frame) {
            if (frame == 1) {
                probe.start(sample, context_a, frame);
                probe.start(second, context_a, frame);
                probe.stop(second, context_a, frame);
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
                probe.start(sample, context_b, frame);
                probe.stop(sample, context_b, frame);
                std::exception_ptr worker_error;
                std::thread worker([&] {
                    try {
                        worker_thread = GetCurrentThreadId();
                        probe.start(sample, context_a, frame);
                        probe.stop(sample, context_a, frame);
                    } catch (...) { worker_error = std::current_exception(); }
                });
                worker.join();
                if (worker_error) std::rethrow_exception(worker_error);
                probe.stop(sample, context_a, frame);
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
            }
            if (frame == 3) probe.start(cross_frame, context_a, frame);
            if (frame == 4) {
                probe.stop(cross_frame, context_a, frame);
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
            }
            if (frame == 5) {
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
                probe.start(sample, context_a, frame);
                probe.stop(sample, context_a, frame);
                probe.start(pending, context_a, frame);
                probe.stop(pending, context_a, frame);
            }
            if (frame == 6) probe.start(pending, context_a, frame);
            const auto before = counter();
            context.step();
            steps.push_back({before, counter()});
        }
        LARGE_INTEGER frequency;
        if (!QueryPerformanceFrequency(&frequency)) throw std::runtime_error("QPC frequency failed");
        std::ofstream out(argv[2]);
        if (!out) throw std::runtime_error("Cannot open truth output");
        out << "{\"sdk\":\"3.4.2\",\"zone_name\":\"PhysXSDK\",\"scene\":" << scene_id
            << ",\"counter_frequency\":" << frequency.QuadPart << ",\"main_thread\":" << main_thread
            << ",\"worker_thread\":" << worker_thread << ",\"context_a\":" << context_a
            << ",\"context_b\":" << context_b << ",\"steps\":[";
        for (std::size_t i = 0; i < steps.size(); ++i) {
            if (i) out << ',';
            out << "{\"frame\":" << i << ",\"lower\":" << steps[i].first << ",\"upper\":" << steps[i].second << '}';
        }
        out << "],\"calls\":[";
        for (std::size_t i = 0; i < probe.calls.size(); ++i) {
            if (i) out << ',';
            write_call(out, probe.calls[i]);
        }
        out << "],\"spans\":[";
        for (std::size_t i = 0; i < probe.spans.size(); ++i) {
            if (i) out << ',';
            const auto& span = probe.spans[i];
            out << "{\"event\":\"" << span.start.event << "\",\"context\":" << span.start.context
                << ",\"thread\":" << span.start.thread << ",\"start_frame\":" << span.start.frame
                << ",\"end_frame\":" << span.end.frame << ",\"start_sequence\":" << span.start.sequence
                << ",\"end_sequence\":" << span.end.sequence << ",\"start_lower\":" << span.start.lower
                << ",\"start_upper\":" << span.start.upper << ",\"end_lower\":" << span.end.lower
                << ",\"end_upper\":" << span.end.upper << '}';
        }
        out << "],\"pending\":[";
        bool first = true;
        for (const auto& entry : probe.pending) {
            for (const auto& start : entry.second) {
                if (!first) out << ',';
                first = false;
                write_call(out, start.call);
            }
        }
        out << "]}\n";
        out.close();
        if (!out) throw std::runtime_error("Cannot flush truth output");
        std::cout << "8 SDK frames, " << probe.calls.size() << " independently bounded calls, "
                  << probe.spans.size() << " complete spans, 1 deliberately pending start\n";
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
