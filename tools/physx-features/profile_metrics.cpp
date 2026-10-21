#define NOMINMAX
#include <Windows.h>
#include "feature_harness.h"
#include "foundation/PxProfiler.h"
#include <atomic>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <map>
#include <mutex>
#include <vector>

static std::uint64_t counter() {
    LARGE_INTEGER value;
    if (!QueryPerformanceCounter(&value)) throw std::runtime_error("QPC failed");
    return static_cast<std::uint64_t>(value.QuadPart);
}

struct Probe : PxProfilerCallback {
    struct Start { unsigned frame; std::uint64_t lower, upper; };
    struct Span { std::string event; std::uint64_t context; Start start; std::uint64_t end_lower, end_upper; };
    PxProfilerCallback* delegate;
    std::atomic<unsigned> frame{0};
    std::mutex mutex;
    std::map<std::pair<std::string, std::uint64_t>, std::vector<Start>> pending;
    std::vector<Span> spans;

    Probe() : delegate(PxGetProfilerCallback()) {
        if (!delegate) throw std::runtime_error("PVD profiler callback is missing");
        PxSetProfilerCallback(this);
    }
    ~Probe() { PxSetProfilerCallback(delegate); }
    static bool relevant(const char* event, bool detached) {
        return detached && (std::string(event) == "Basic.simulate" || std::string(event) == "Basic.collision" || std::string(event) == "Basic.rigidBodySolver");
    }
    void* zoneStart(const char* event, bool detached, std::uint64_t context) override {
        if (!relevant(event, detached)) return delegate->zoneStart(event, detached, context);
        const auto lower = counter();
        auto* result = delegate->zoneStart(event, detached, context);
        const auto upper = counter();
        std::lock_guard<std::mutex> guard(mutex);
        pending[{event, context}].push_back({frame.load(), lower, upper});
        return result;
    }
    void zoneEnd(void* data, const char* event, bool detached, std::uint64_t context) override {
        if (!relevant(event, detached)) { delegate->zoneEnd(data, event, detached, context); return; }
        const auto lower = counter();
        delegate->zoneEnd(data, event, detached, context);
        const auto upper = counter();
        std::lock_guard<std::mutex> guard(mutex);
        auto& starts = pending[{event, context}];
        if (starts.empty()) throw std::runtime_error("Unmatched probed profile stop");
        spans.push_back({event, context, starts.back(), lower, upper});
        starts.pop_back();
    }
};

int main(int argc, char** argv) {
    if (argc != 3) { std::cerr << "usage: pvd-profile-metrics capture.pvdstream truth.json\n"; return 2; }
    try {
        FeatureContext context(argv[1]);
        auto* ground = feature_require(PxCreatePlane(*context.physics, PxPlane(0, 1, 0, 0), *context.material), "ground");
        auto* box = feature_require(PxCreateDynamic(*context.physics, PxTransform(PxVec3(0, 2, 0)), PxBoxGeometry(PxVec3(0.5f)), *context.material, 1.0f), "box");
        context.scene->addActor(*ground);
        context.scene->addActor(*box);
        Probe probe;
        std::vector<std::pair<std::uint64_t, std::uint64_t>> steps;
        for (unsigned frame = 0; frame < 24; ++frame) {
            probe.frame.store(frame);
            const auto before = counter();
            context.step();
            steps.push_back({before, counter()});
        }
        LARGE_INTEGER frequency;
        if (!QueryPerformanceFrequency(&frequency)) throw std::runtime_error("QPC frequency failed");
        std::ofstream out(argv[2]);
        if (!out) throw std::runtime_error("Cannot open truth output");
        out << "{\"sdk\":\"3.4.2\",\"scene\":" << reinterpret_cast<std::uintptr_t>(context.scene)
            << ",\"counter_frequency\":" << frequency.QuadPart << ",\"steps\":[";
        for (std::size_t i = 0; i < steps.size(); ++i) {
            if (i) out << ',';
            out << "{\"frame\":" << i << ",\"lower\":" << steps[i].first << ",\"upper\":" << steps[i].second << '}';
        }
        out << "],\"spans\":[";
        for (std::size_t i = 0; i < probe.spans.size(); ++i) {
            if (i) out << ',';
            const auto& span = probe.spans[i];
            out << "{\"event\":\"" << span.event << "\",\"context\":" << span.context << ",\"frame\":" << span.start.frame
                << ",\"start_lower\":" << span.start.lower << ",\"start_upper\":" << span.start.upper
                << ",\"end_lower\":" << span.end_lower << ",\"end_upper\":" << span.end_upper << '}';
        }
        out << "]}\n";
        out.close();
        if (!out) throw std::runtime_error("Cannot flush truth output");
        std::cout << "24 SDK frames, " << probe.spans.size() << " independently timed profile callbacks\n";
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
