/* Read-only KMS inventory for bench-native-gpu.py. Opens the requested node
 * directly: driver-name lookup can select the wrong GPU on multi-GPU hosts.
 * Build: cc -O2 -Wall -Wextra scripts/native-drm-info.c $(pkg-config --cflags --libs libdrm) -o /tmp/native-drm-info
 */
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

int main(int argc, char **argv) {
    if (argc != 2) { fprintf(stderr, "usage: %s /dev/dri/cardN\n", argv[0]); return 2; }
    int fd = open(argv[1], O_RDWR | O_CLOEXEC);
    if (fd < 0) { perror("open DRM node"); return 1; }
    if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1)) {
        perror("universal planes"); close(fd); return 1;
    }
    drmModeRes *res = drmModeGetResources(fd);
    if (!res) { perror("KMS resources"); close(fd); return 1; }
    printf("{\"crtcs\":[");
    for (int i = 0; i < res->count_crtcs; ++i) {
        drmModeCrtc *c = drmModeGetCrtc(fd, res->crtcs[i]);
        if (!c) { perror("KMS CRTC"); return 1; }
        uint64_t sequence = 0, ns = 0;
        int seq_status = drmCrtcGetSequence(fd, c->crtc_id, &sequence, &ns);
        double hz = c->mode_valid && c->mode.htotal && c->mode.vtotal
            ? (double)c->mode.clock * 1000 / c->mode.htotal / c->mode.vtotal : 0;
        printf("%s{\"id\":%u,\"active\":%s,\"width\":%u,\"height\":%u,\"hz\":%.6f,\"sequence_available\":%s,\"sequence\":%" PRIu64 ",\"sequence_ns\":%" PRIu64 "}",
            i ? "," : "", c->crtc_id, c->mode_valid ? "true" : "false", c->width, c->height, hz,
            seq_status == 0 ? "true" : "false", sequence, ns);
        drmModeFreeCrtc(c);
    }
    printf("],\"connectors\":[");
    for (int i = 0; i < res->count_connectors; ++i) {
        drmModeConnector *c = drmModeGetConnector(fd, res->connectors[i]);
        if (!c) { perror("KMS connector"); return 1; }
        printf("%s{\"id\":%u,\"type\":%u,\"type_id\":%u,\"connected\":%s,\"modes\":%d}",
            i ? "," : "", c->connector_id, c->connector_type, c->connector_type_id,
            c->connection == DRM_MODE_CONNECTED ? "true" : "false", c->count_modes);
        drmModeFreeConnector(c);
    }
    printf("],\"planes\":[");
    drmModePlaneRes *planes = drmModeGetPlaneResources(fd);
    if (!planes) { perror("KMS planes"); return 1; }
    for (uint32_t i = 0; i < planes->count_planes; ++i) {
        drmModePlane *p = drmModeGetPlane(fd, planes->planes[i]);
        if (!p) { perror("KMS plane"); return 1; }
        drmModeObjectProperties *props = drmModeObjectGetProperties(fd, p->plane_id, DRM_MODE_OBJECT_PLANE);
        uint64_t type = UINT64_MAX;
        if (props) {
            for (uint32_t j = 0; j < props->count_props; ++j) {
                drmModePropertyRes *prop = drmModeGetProperty(fd, props->props[j]);
                if (prop) {
                    if (!strcmp(prop->name, "type")) type = props->prop_values[j];
                    drmModeFreeProperty(prop);
                }
            }
            drmModeFreeObjectProperties(props);
        }
        printf("%s{\"id\":%u,\"type\":%" PRIu64 ",\"crtc\":%u,\"framebuffer\":%u,\"possible_crtcs\":%u,\"formats\":[",
            i ? "," : "", p->plane_id, type, p->crtc_id, p->fb_id, p->possible_crtcs);
        for (uint32_t j = 0; j < p->count_formats; ++j) printf("%s%u", j ? "," : "", p->formats[j]);
        printf("]}"); drmModeFreePlane(p);
    }
    puts("]}");
    drmModeFreePlaneResources(planes); drmModeFreeResources(res); close(fd);
    return 0;
}
