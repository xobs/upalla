// C helper: build spa_pod format params for PipeWire filter.

#include <spa/param/audio/format-utils.h>
#include <spa/param/latency-utils.h>
#include <spa/pod/builder.h>
#include <stdlib.h>

struct PwFormatParams {
    uint8_t buffer[4096];
    const struct spa_pod *params[3];
    uint32_t n_params;
};

struct PwFormatParams *upalla_build_format_params(void) {
    struct PwFormatParams *p = calloc(1, sizeof(*p));
    if (!p) return NULL;

    struct spa_pod_builder b;
    spa_pod_builder_init(&b, p->buffer, sizeof(p->buffer));

    struct spa_process_latency_info latency = {
        .ns = 40 * 1000000,
    };
    p->params[0] = spa_process_latency_build(&b, SPA_PARAM_ProcessLatency, &latency);

    struct spa_audio_info_raw info = {0};
    info.format = SPA_AUDIO_FORMAT_F32;
    info.channels = 2;
    info.rate = 48000;

    p->params[1] = spa_format_audio_raw_build(&b, SPA_PARAM_EnumFormat, &info);
    p->n_params = 2;

    return p;
}

void upalla_free_format_params(struct PwFormatParams *p) {
    free(p);
}
