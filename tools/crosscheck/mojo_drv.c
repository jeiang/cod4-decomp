/* SPDX-License-Identifier: GPL-3.0-only */
/* Cross-check driver: MojoShader (zlib) D3D9 bytecode -> SPIR-V, linked per (vs, ps) pair.
   usage: mojo_drv pairs.txt blobdir outdir     pairs.txt lines: "<vshash> <pshash>"
   reads  <blobdir>/vs_<h>.bin, ps_<h>.bin
   writes <outdir>/vs_<vh>__<ph>.spv, ps_<ph>__<vh>.spv
   stdout: "pair <vh> <ph> OK|ERR <msg>" */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "mojoshader.h"

static unsigned char *rd(const char *p, long *n) {
  FILE *f = fopen(p, "rb");
  if (!f) return NULL;
  fseek(f, 0, SEEK_END); *n = ftell(f); fseek(f, 0, SEEK_SET);
  unsigned char *b = malloc(*n);
  if (fread(b, 1, *n, f) != (size_t)*n) { fclose(f); free(b); return NULL; }
  fclose(f);
  return b;
}
static void wr(const char *p, const void *d, size_t n) {
  FILE *f = fopen(p, "wb");
  if (!f) { perror(p); exit(2); }
  fwrite(d, 1, n, f); fclose(f);
}
int main(int c, char **v) {
  if (c != 4) { fprintf(stderr, "usage: mojo_drv pairs.txt blobdir outdir\n"); return 2; }
  FILE *pf = fopen(v[1], "r");
  if (!pf) { perror(v[1]); return 2; }
  char vh[64], ph[64];
  while (fscanf(pf, "%63s %63s", vh, ph) == 2) {
    char path[512]; long n1 = 0, n2 = 0;
    snprintf(path, sizeof path, "%s/vs_%s.bin", v[2], vh); unsigned char *vb = rd(path, &n1);
    snprintf(path, sizeof path, "%s/ps_%s.bin", v[2], ph); unsigned char *pb = rd(path, &n2);
    if (!vb || !pb) { printf("pair %s %s ERR unreadable blob\n", vh, ph); free(vb); free(pb); continue; }
    const MOJOSHADER_parseData *vs = MOJOSHADER_parse("spirv", NULL, vb, n1, NULL, 0, NULL, 0, NULL, NULL, NULL);
    const MOJOSHADER_parseData *ps = MOJOSHADER_parse("spirv", NULL, pb, n2, NULL, 0, NULL, 0, NULL, NULL, NULL);
    int ok = 1;
    if (vs->error_count) { printf("pair %s %s ERR vs-parse: %s\n", vh, ph, vs->errors[0].error); ok = 0; }
    if (ps->error_count) { printf("pair %s %s ERR ps-parse: %s\n", vh, ph, ps->errors[0].error); ok = 0; }
    if (ok) {
      MOJOSHADER_vertexAttribute a[16];
      int na = vs->attribute_count > 16 ? 16 : vs->attribute_count;
      for (int i = 0; i < na; i++) {
        a[i].usage = vs->attributes[i].usage;
        a[i].usageIndex = vs->attributes[i].index;
        a[i].vertexElementFormat = MOJOSHADER_VERTEXELEMENTFORMAT_VECTOR4;
      }
      int pt = MOJOSHADER_linkSPIRVShaders(vs, ps, a, na);
      if (pt < 0) { printf("pair %s %s ERR link %d\n", vh, ph, pt); }
      else {
        snprintf(path, sizeof path, "%s/vs_%s__%s.spv", v[3], vh, ph); wr(path, vs->output, vs->output_len - pt);
        snprintf(path, sizeof path, "%s/ps_%s__%s.spv", v[3], ph, vh); wr(path, ps->output, ps->output_len - pt);
        printf("pair %s %s OK\n", vh, ph);
      }
    }
    MOJOSHADER_freeParseData(vs); MOJOSHADER_freeParseData(ps); free(vb); free(pb);
  }
  return 0;
}
