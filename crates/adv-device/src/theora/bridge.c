/* Host-owned Theora state. Guest addresses never enter the decoder. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "decint.h"

typedef struct {
  int64_t keyframe_num;
  int64_t curframe_num;
  int64_t granpos;
  int32_t pp_level;
  uint8_t frame_type;
} AdvTheoraState;

typedef struct {
  const int16_t *huffman[80];
  uint16_t dc_scale[64];
  uint16_t ac_scale[64];
  unsigned char loop_filter_limits[64];
  th_quant_ranges ranges[2][3];
} AdvTheoraSetup;

void *adv_theora_create(const unsigned char *info, const AdvTheoraSetup *input) {
  th_info parsed;
  th_setup_info setup;
  memcpy(&parsed, info, sizeof(parsed));
  memcpy(setup.huff_tables, input->huffman, sizeof(setup.huff_tables));
  memcpy(setup.qinfo.dc_scale, input->dc_scale, sizeof(setup.qinfo.dc_scale));
  memcpy(setup.qinfo.ac_scale, input->ac_scale, sizeof(setup.qinfo.ac_scale));
  memcpy(setup.qinfo.loop_filter_limits, input->loop_filter_limits,
   sizeof(setup.qinfo.loop_filter_limits));
  memcpy(setup.qinfo.qi_ranges, input->ranges, sizeof(setup.qinfo.qi_ranges));
  th_dec_ctx *decoder = th_decode_alloc(&parsed, &setup);
#ifdef ADV_THEORA_NEON
  void adv_theora_accel_init(th_dec_ctx *);
  void adv_theora_dispatch(void *);
  if (decoder != NULL) {
    adv_theora_accel_init(decoder);
    decoder->adv_dispatch = (void (*)(th_dec_ctx *))adv_theora_dispatch;
  }
#endif
  return decoder;
}

size_t adv_theora_info_size(void) {
  return sizeof(th_info);
}

void adv_theora_destroy(void *decoder) {
  th_decode_free((th_dec_ctx *)decoder);
}

int adv_theora_decode(void *opaque, const unsigned char *data, size_t bytes,
 int64_t packetno, int64_t granulepos, int eos, int striped, AdvTheoraState *state) {
  th_dec_ctx *decoder = (th_dec_ctx *)opaque;
  ogg_packet packet;
  int result;
  memset(&packet, 0, sizeof(packet));
  packet.packet = (unsigned char *)data;
  packet.bytes = (long)bytes;
  packet.packetno = packetno;
  packet.granulepos = granulepos;
  packet.e_o_s = eos;
  decoder->state.keyframe_num = state->keyframe_num;
  decoder->state.curframe_num = state->curframe_num;
  decoder->state.granpos = state->granpos;
  decoder->pp_level = state->pp_level;
  decoder->adv_stripe_yield = striped;
  result = th_decode_packetin(decoder, &packet, &state->granpos);
  state->keyframe_num = decoder->state.keyframe_num;
  state->curframe_num = decoder->state.curframe_num;
  state->frame_type = decoder->state.frame_type;
  return result;
}

void adv_theora_planes(void *opaque, th_img_plane planes[3]) {
  th_dec_ctx *decoder = (th_dec_ctx *)opaque;
  memcpy(planes, decoder->pp_frame_buf, sizeof(th_ycbcr_buffer));
}

void adv_theora_stripe(void *opaque, int range[2]) {
  th_dec_ctx *decoder = (th_dec_ctx *)opaque;
  range[0] = decoder->adv_yfrag0;
  range[1] = decoder->adv_yfrag_end;
}

unsigned char adv_theora_info(void *opaque, unsigned char info[64]) {
  th_dec_ctx *decoder = (th_dec_ctx *)opaque;
  memcpy(info, &decoder->state.info, sizeof(th_info));
  return decoder->state.granpos_bias;
}
