// moonlight-common-c takes its callbacks as C structs whose layout the Rust
// side would otherwise have to mirror exactly. This shim owns those structs
// and exposes a flat set of function pointers instead; it also flattens each
// decode unit's buffer chain into one Annex B buffer.
#pragma once

#include <stdbool.h>
#include <stdint.h>

typedef struct bl_callbacks {
    int (*video_setup)(void* ctx, int video_format, int width, int height, int fps);
    void (*video_cleanup)(void* ctx);
    // One access unit in Annex B. frame_type: 0 = P, 1 = IDR. Return 0, or
    // -1 to ask the host for a fresh IDR frame.
    int (*video_frame)(void* ctx, const uint8_t* data, int len, int frame_type, int frame_number,
                       uint16_t host_latency_tenths_ms, uint64_t receive_us, uint64_t enqueue_us);
    int (*audio_setup)(void* ctx, int sample_rate, int channels, int streams, int coupled,
                       int samples_per_frame, const uint8_t* mapping);
    void (*audio_cleanup)(void* ctx);
    void (*audio_packet)(void* ctx, const uint8_t* data, int len);
    // state: 0 starting, 1 complete, 2 failed (error set).
    void (*stage)(void* ctx, int stage, int state, int error);
    void (*connected)(void* ctx);
    void (*terminated)(void* ctx, int error);
    void (*status)(void* ctx, int status);
    void (*log)(void* ctx, const char* line);
} bl_callbacks;

typedef struct bl_stream_config {
    int width;
    int height;
    int fps;
    int bitrate_kbps;
    int packet_size;
    int remote;  // STREAM_CFG_*
    int video_formats;  // VIDEO_FORMAT_* mask
    int color_space;
    int color_range;
    int encryption_flags;
    int video_capabilities;
    int audio_capabilities;
    const uint8_t* ri_key;  // 16 bytes
    const uint8_t* ri_iv;  // 16 bytes
} bl_stream_config;

typedef struct bl_server_info {
    const char* address;
    const char* app_version;
    const char* gfe_version;
    const char* rtsp_url;
    int codec_mode_support;
} bl_server_info;

int bl_start(const bl_server_info* server, const bl_stream_config* cfg, const bl_callbacks* cb, void* ctx);
void bl_stop(void);
void bl_interrupt(void);
const char* bl_stage_name(int stage);
const char* bl_launch_query(void);
