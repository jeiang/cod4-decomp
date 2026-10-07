/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Cross-check driver: vkd3d-shader (LGPL-2.1+, linked as a separate program) d3dbc -> SPIR-V (Vulkan 1.0 env).
   usage: vkd3d_drv list.txt blobdir outdir      list lines: "<vs|ps> <hash>"
   reads <blobdir>/<kind>_<h>.bin, writes <outdir>/<kind>_<h>.spv
   stdout: "<kind> <hash> OK|ERR <first message line>" */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <vkd3d_shader.h>

static unsigned char *rd(const char *p, long *n) {
  FILE *f = fopen(p, "rb");
  if (!f) return NULL;
  fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET);
  unsigned char *b = malloc(*n);
  if (fread(b, 1, *n, f) != (size_t)*n) { fclose(f); free(b); return NULL; }
  fclose(f);
  return b;
}
int main(int c, char **v) {
  if (c != 4) { fprintf(stderr, "usage: vkd3d_drv list.txt blobdir outdir\n"); return 2; }
  FILE *lf = fopen(v[1], "r");
  if (!lf) { perror(v[1]); return 2; }
  char kind[8], h[64];
  while (fscanf(lf, "%7s %63s", kind, h) == 2) {
    char path[512]; long n = 0;
    snprintf(path, sizeof path, "%s/%s_%s.bin", v[2], kind, h);
    unsigned char *b = rd(path, &n);
    if (!b) { printf("%s %s ERR unreadable blob\n", kind, h); continue; }
    struct vkd3d_shader_spirv_target_info spv = {0};
    spv.type = VKD3D_SHADER_STRUCTURE_TYPE_SPIRV_TARGET_INFO;
    spv.environment = VKD3D_SHADER_SPIRV_ENVIRONMENT_VULKAN_1_0;
    struct vkd3d_shader_compile_info ci = {0};
    ci.type = VKD3D_SHADER_STRUCTURE_TYPE_COMPILE_INFO; ci.next = &spv;
    ci.source.code = b; ci.source.size = n;
    ci.source_type = VKD3D_SHADER_SOURCE_D3D_BYTECODE; ci.target_type = VKD3D_SHADER_TARGET_SPIRV_BINARY;
    ci.log_level = VKD3D_SHADER_LOG_WARNING;
    struct vkd3d_shader_code out = {0}; char *msg = NULL;
    int r = vkd3d_shader_compile(&ci, &out, &msg);
    if (r < 0) {
      char *e = msg ? msg : "(no message)"; char *nl = strchr(e, '\n'); if (nl) *nl = 0;
      printf("%s %s ERR rc=%d %s\n", kind, h, r, e);
    } else {
      snprintf(path, sizeof path, "%s/%s_%s.spv", v[3], kind, h);
      FILE *o = fopen(path, "wb"); if (!o) { perror(path); return 2; }
      fwrite(out.code, 1, out.size, o); fclose(o);
      printf("%s %s OK\n", kind, h);
      vkd3d_shader_free_shader_code(&out);
    }
    if (msg) vkd3d_shader_free_messages(msg);
    free(b);
  }
  return 0;
}
