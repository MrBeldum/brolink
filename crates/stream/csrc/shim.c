#include "shim.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "Limelight.h"

static bl_callbacks g_cb;
static void* g_ctx;
static uint8_t* g_frame;
static int g_frame_cap;

static int dr_setup(int videoFormat, int width, int height, int redrawRate, void* context, int drFlags) {
    (void)context;
    (void)drFlags;
    return g_cb.video_setup(g_ctx, videoFormat, width, height, redrawRate);
}

static void dr_cleanup(void) {
    g_cb.video_cleanup(g_ctx);
    free(g_frame);
    g_frame = NULL;
    g_frame_cap = 0;
}

static int dr_submit(PDECODE_UNIT du) {
    if (du->fullLength > g_frame_cap) {
        int cap = du->fullLength + du->fullLength / 2;
        uint8_t* p = realloc(g_frame, cap);
        if (!p) {
            return DR_NEED_IDR;
        }
        g_frame = p;
        g_frame_cap = cap;
    }
    int off = 0;
    for (PLENTRY e = du->bufferList; e != NULL; e = e->next) {
        memcpy(g_frame + off, e->data, e->length);
        off += e->length;
    }
    return g_cb.video_frame(g_ctx, g_frame, off, du->frameType, du->frameNumber,
                            du->frameHostProcessingLatency, du->receiveTimeUs, du->enqueueTimeUs);
}

static int ar_init(int audioConfiguration, const POPUS_MULTISTREAM_CONFIGURATION o, void* context, int arFlags) {
    (void)audioConfiguration;
    (void)context;
    (void)arFlags;
    return g_cb.audio_setup(g_ctx, o->sampleRate, o->channelCount, o->streams, o->coupledStreams,
                            o->samplesPerFrame, o->mapping);
}

static void ar_cleanup(void) {
    g_cb.audio_cleanup(g_ctx);
}

static void ar_sample(char* data, int len) {
    g_cb.audio_packet(g_ctx, (const uint8_t*)data, len);
}

static void cl_stage_starting(int stage) {
    g_cb.stage(g_ctx, stage, 0, 0);
}

static void cl_stage_complete(int stage) {
    g_cb.stage(g_ctx, stage, 1, 0);
}

static void cl_stage_failed(int stage, int err) {
    g_cb.stage(g_ctx, stage, 2, err);
}

static void cl_connected(void) {
    g_cb.connected(g_ctx);
}

static void cl_terminated(int err) {
    g_cb.terminated(g_ctx, err);
}

static void cl_status(int status) {
    g_cb.status(g_ctx, status);
}

static void cl_log(const char* fmt, ...) {
    char buf[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof buf, fmt, ap);
    va_end(ap);
    size_t n = strlen(buf);
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) {
        buf[--n] = 0;
    }
    if (n > 0) {
        g_cb.log(g_ctx, buf);
    }
}

int bl_start(const bl_server_info* server, const bl_stream_config* cfg, const bl_callbacks* cb, void* ctx) {
    g_cb = *cb;
    g_ctx = ctx;

    SERVER_INFORMATION si;
    LiInitializeServerInformation(&si);
    si.address = server->address;
    si.serverInfoAppVersion = server->app_version;
    si.serverInfoGfeVersion = server->gfe_version;
    si.rtspSessionUrl = server->rtsp_url;
    si.serverCodecModeSupport = server->codec_mode_support;

    STREAM_CONFIGURATION sc;
    LiInitializeStreamConfiguration(&sc);
    sc.width = cfg->width;
    sc.height = cfg->height;
    sc.fps = cfg->fps;
    sc.bitrate = cfg->bitrate_kbps;
    sc.packetSize = cfg->packet_size;
    sc.streamingRemotely = cfg->remote;
    sc.audioConfiguration = AUDIO_CONFIGURATION_STEREO;
    sc.supportedVideoFormats = cfg->video_formats;
    sc.clientRefreshRateX100 = cfg->fps * 100;
    sc.colorSpace = cfg->color_space;
    sc.colorRange = cfg->color_range;
    sc.encryptionFlags = cfg->encryption_flags;
    memcpy(sc.remoteInputAesKey, cfg->ri_key, 16);
    memcpy(sc.remoteInputAesIv, cfg->ri_iv, 16);

    DECODER_RENDERER_CALLBACKS dr;
    LiInitializeVideoCallbacks(&dr);
    dr.setup = dr_setup;
    dr.cleanup = dr_cleanup;
    dr.submitDecodeUnit = dr_submit;
    dr.capabilities = cfg->video_capabilities;

    AUDIO_RENDERER_CALLBACKS ar;
    LiInitializeAudioCallbacks(&ar);
    ar.init = ar_init;
    ar.cleanup = ar_cleanup;
    ar.decodeAndPlaySample = ar_sample;
    ar.capabilities = cfg->audio_capabilities;

    CONNECTION_LISTENER_CALLBACKS cl;
    LiInitializeConnectionCallbacks(&cl);
    cl.stageStarting = cl_stage_starting;
    cl.stageComplete = cl_stage_complete;
    cl.stageFailed = cl_stage_failed;
    cl.connectionStarted = cl_connected;
    cl.connectionTerminated = cl_terminated;
    cl.connectionStatusUpdate = cl_status;
    cl.logMessage = cl_log;

    return LiStartConnection(&si, &sc, &cl, &dr, &ar, NULL, 0, NULL, 0);
}

void bl_stop(void) {
    LiStopConnection();
}

void bl_interrupt(void) {
    LiInterruptConnection();
}

const char* bl_stage_name(int stage) {
    return LiGetStageName(stage);
}

const char* bl_launch_query(void) {
    return LiGetLaunchUrlQueryParameters();
}
