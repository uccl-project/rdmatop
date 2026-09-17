#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void (*rdmatop_sample_fn)(void* ctx, uint64_t ts_ns, const char* dev,
                                  uint32_t port, const char* metric, double value);

void* rdmatop_capture_start(uint64_t interval_us, const char* devices_csv);
void rdmatop_capture_stop(void* handle);
void rdmatop_capture_for_each(void* handle, rdmatop_sample_fn cb, void* ctx);
const char* rdmatop_capture_error(void* handle);
void rdmatop_capture_free(void* handle);
const char* rdmatop_last_error(void);

#ifdef __cplusplus
}
#endif
