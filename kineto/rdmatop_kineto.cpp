#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <utility>
#include <vector>

#include <ActivityType.h>
#include <GenericTraceActivity.h>
#include <IActivityProfiler.h>
#include <libkineto.h>
#include <output_base.h>

#include "rdmatop_capture.h"

namespace {

constexpr int32_t kDeviceId = 0x7fff0000;
constexpr int64_t kSortIndex = 0x7fff0000;
constexpr double kDefaultIntervalMs = 10.0;
constexpr double kMaxIntervalMs = 3.6e6;
const std::string kName = "rdmatop";

double intervalMsFromEnv() {
  const char* value = std::getenv("RDMATOP_INTERVAL_MS");
  if (!value) {
    return kDefaultIntervalMs;
  }
  double ms = std::atof(value);
  if (std::isnan(ms) || ms <= 0.0) {
    return kDefaultIntervalMs;
  }
  return std::min(ms, kMaxIntervalMs);
}

uint64_t intervalMicrosFromEnv() {
  return static_cast<uint64_t>(intervalMsFromEnv() * 1000.0);
}

struct Collector {
  std::vector<libkineto::GenericTraceActivity> activities;
  std::map<std::string, int32_t> resources;
  uint64_t lastTsNs = 0;
  std::string lastName;
  bool addFailed = false;

  static void onSample(void* ctx, uint64_t tsNs, const char* dev, uint32_t port,
                       const char* metric, double value) {
    auto* self = static_cast<Collector*>(ctx);
    try {
      self->add(tsNs, dev, port, metric, value);
    } catch (...) {
      self->addFailed = true;
    }
  }

  void add(uint64_t tsNs, const char* dev, uint32_t port, const char* metric, double value) {
    std::string name = std::string(dev) + ":" + std::to_string(port);
    if (!continuesLast(tsNs, name)) {
      startActivity(tsNs, name);
    }
#if RDMATOP_TYPED_COUNTERS
    activities.back().addCounterValue(metric, value);
#else
    activities.back().addMetadata(metric, value);
#endif
  }

  bool continuesLast(uint64_t tsNs, const std::string& name) const {
    return !activities.empty() && lastTsNs == tsNs && lastName == name;
  }

  void startActivity(uint64_t tsNs, const std::string& name) {
    libkineto::GenericTraceActivity activity;
#if RDMATOP_NATIVE_COUNTERS
    activity.activityType = libkineto::ActivityType::MTIA_COUNTERS;
#else
    // CPU event types are reserved for events owned by the PyTorch profiler.
    activity.activityType = libkineto::ActivityType::PRIVATEUSE1_RUNTIME;
    activity.addMetadata("rdmatop_counter", 1);
#endif
    activity.activityName = name;
    activity.startTime = static_cast<int64_t>(tsNs);
    activity.endTime = static_cast<int64_t>(tsNs);
    activity.device = kDeviceId;
    activity.resource = resourceFor(name);
    activities.push_back(std::move(activity));
    lastTsNs = tsNs;
    lastName = name;
  }

  int32_t resourceFor(const std::string& name) {
    auto found = resources.find(name);
    if (found != resources.end()) {
      return found->second;
    }
    int32_t id = static_cast<int32_t>(resources.size());
    resources.emplace(name, id);
    return id;
  }
};

using CaptureHandle = std::unique_ptr<void, void (*)(void*)>;

class RdmatopSession : public libkineto::IActivityProfilerSession {
 public:
  void start() override {
    if (handle_) {
      return;
    }
    handle_.reset(rdmatop_capture_start(intervalMicrosFromEnv(), std::getenv("RDMATOP_DEVICES")));
    if (!handle_) {
      const char* message = rdmatop_last_error();
      fail(message ? message : "capture failed");
    }
  }

  void stop() override {
    if (!handle_) {
      return;
    }
    rdmatop_capture_stop(handle_.get());
    rdmatop_capture_for_each(handle_.get(), &Collector::onSample, &collector_);
    if (const char* message = rdmatop_capture_error(handle_.get())) {
      fail(message);
    }
    if (collector_.addFailed) {
      fail("failed to collect samples");
    }
    handle_.reset();
  }

  std::vector<std::string> errors() override {
    return errors_;
  }

  void processTrace(libkineto::ActivityLogger& logger) override {
    for (const auto& activity : collector_.activities) {
      logger.handleGenericActivity(activity);
    }
  }

  std::unique_ptr<libkineto::DeviceInfo> getDeviceInfo() override {
    if (collector_.activities.empty()) {
      return nullptr;
    }
    return std::make_unique<libkineto::DeviceInfo>(
        libkineto::DeviceInfo{kDeviceId, kSortIndex, kName, kName});
  }

  std::vector<libkineto::ResourceInfo> getResourceInfos() override {
    std::vector<libkineto::ResourceInfo> infos;
    for (const auto& [name, id] : collector_.resources) {
      // Older Kineto constructors and newer aggregates order the IDs differently.
      libkineto::ResourceInfo info{0, 0, 0, name};
      info.deviceId = kDeviceId;
      info.id = id;
      info.sortIndex = id;
      infos.push_back(std::move(info));
    }
    return infos;
  }

  std::unique_ptr<libkineto::CpuTraceBuffer> getTraceBuffer() override {
    return nullptr;
  }

 private:
  void fail(const std::string& message) {
    std::fprintf(stderr, "rdmatop: %s\n", message.c_str());
    errors_.push_back("rdmatop: " + message);
  }

  CaptureHandle handle_{nullptr, rdmatop_capture_free};
  Collector collector_;
  std::vector<std::string> errors_;
};

class RdmatopProfiler : public libkineto::IActivityProfiler {
 public:
  const std::string& name() const override {
    return kName;
  }

  const std::set<libkineto::ActivityType>& availableActivities() const override {
    return activities_;
  }

  std::unique_ptr<libkineto::IActivityProfilerSession> configure(
      const std::set<libkineto::ActivityType>& types,
      const libkineto::Config&) override {
    if (types.count(libkineto::ActivityType::CPU_OP) == 0) {
      return nullptr;
    }
    return std::make_unique<RdmatopSession>();
  }

  std::unique_ptr<libkineto::IActivityProfilerSession> configure(
      int64_t, int64_t, const std::set<libkineto::ActivityType>& types,
      const libkineto::Config& config) override {
    return configure(types, config);
  }

 private:
  std::set<libkineto::ActivityType> activities_{libkineto::ActivityType::CPU_OP};
};

bool registered = false;

}  // namespace

extern "C" int rdmatop_kineto_has_native_counters(void) {
  return RDMATOP_NATIVE_COUNTERS;
}

extern "C" int rdmatop_kineto_register(void) {
  if (registered) {
    return 0;
  }
  registered = true;
  libkineto::api().registerProfilerFactory(
      [] { return std::make_unique<RdmatopProfiler>(); });
  return 0;
}
