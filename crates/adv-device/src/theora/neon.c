/* Exact ARM64 reconstruction and IDCT for the Xiph decoder's native ABI.
 * Transform/filter equations: Copyright (C) 2002-2009,2025 Xiph.Org Foundation
 * and contributors, BSD-3-Clause; see vendor/libtheora/COPYING.
 */
#include <arm_neon.h>
#include <string.h>
#include "decint.h"
#include "dct.h"

void adv_theora_copy(unsigned char *dst,const unsigned char *src,int stride) {
  for(int y=0;y<8;y++) {
    vst1_u8(dst,vld1_u8(src));
    dst+=stride;src+=stride;
  }
}
void adv_theora_copy_list(unsigned char *dst,const unsigned char *src,int stride,
 const ptrdiff_t *indices,ptrdiff_t count,const ptrdiff_t *offsets) {
  for(ptrdiff_t i=0;i<count;i++) {
    ptrdiff_t offset=offsets[indices[i]];
    adv_theora_copy(dst+offset,src+offset,stride);
  }
}
void adv_theora_intra(unsigned char *dst,int stride,const ogg_int16_t residue[64]) {
  for(int y=0;y<8;y++) {
    int16x8_t value=vqaddq_s16(vld1q_s16(residue+y*8),vdupq_n_s16(128));
    vst1_u8(dst,vqmovun_s16(value));dst+=stride;
  }
}
void adv_theora_inter(unsigned char *dst,const unsigned char *src,int stride,
 const ogg_int16_t residue[64]) {
  for(int y=0;y<8;y++) {
    int16x8_t predicted=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(src)));
    vst1_u8(dst,vqmovun_s16(vqaddq_s16(vld1q_s16(residue+y*8),predicted)));
    dst+=stride;src+=stride;
  }
}
void adv_theora_inter2(unsigned char *dst,const unsigned char *src1,
 const unsigned char *src2,int stride,const ogg_int16_t residue[64]) {
  for(int y=0;y<8;y++) {
    uint8x8_t average=vhadd_u8(vld1_u8(src1),vld1_u8(src2));
    int16x8_t predicted=vreinterpretq_s16_u16(vmovl_u8(average));
    vst1_u8(dst,vqmovun_s16(vqaddq_s16(vld1q_s16(residue+y*8),predicted)));
    dst+=stride;src1+=stride;src2+=stride;
  }
}

static inline int16x8_t mulhi(int16x8_t x,int coefficient) {
  int16_t c=(int16_t)coefficient;
  int16x8_t result=vcombine_s16(vshrn_n_s32(vmull_n_s16(vget_low_s16(x),c),16),
   vshrn_n_s32(vmull_n_s16(vget_high_s16(x),c),16));
  return coefficient>32767?vaddq_s16(result,x):result;
}
static inline void transpose(int16x8_t r[8]) {
  int16x8_t t[8],u[8];
  for(int i=0;i<8;i+=2) {
    int16x8x2_t p=vtrnq_s16(r[i],r[i+1]);t[i]=p.val[0];t[i+1]=p.val[1];
  }
  for(int i=0;i<8;i+=4) {
    int32x4x2_t a=vtrnq_s32(vreinterpretq_s32_s16(t[i]),vreinterpretq_s32_s16(t[i+2]));
    int32x4x2_t b=vtrnq_s32(vreinterpretq_s32_s16(t[i+1]),vreinterpretq_s32_s16(t[i+3]));
    u[i]=vreinterpretq_s16_s32(a.val[0]);u[i+2]=vreinterpretq_s16_s32(a.val[1]);
    u[i+1]=vreinterpretq_s16_s32(b.val[0]);u[i+3]=vreinterpretq_s16_s32(b.val[1]);
  }
  for(int i=0;i<4;i++) {
    r[i]=vcombine_s16(vget_low_s16(u[i]),vget_low_s16(u[i+4]));
    r[i+4]=vcombine_s16(vget_high_s16(u[i]),vget_high_s16(u[i+4]));
  }
}
static inline void idct8(int16x8_t x[8]) {
  int16x8_t t[8],r;
  t[0]=mulhi(vaddq_s16(x[0],x[4]),OC_C4S4);
  t[1]=mulhi(vsubq_s16(x[0],x[4]),OC_C4S4);
  t[2]=vsubq_s16(mulhi(x[2],OC_C6S2),mulhi(x[6],OC_C2S6));
  t[3]=vaddq_s16(mulhi(x[2],OC_C2S6),mulhi(x[6],OC_C6S2));
  t[4]=vsubq_s16(mulhi(x[1],OC_C7S1),mulhi(x[7],OC_C1S7));
  t[5]=vsubq_s16(mulhi(x[5],OC_C3S5),mulhi(x[3],OC_C5S3));
  t[6]=vaddq_s16(mulhi(x[5],OC_C5S3),mulhi(x[3],OC_C3S5));
  t[7]=vaddq_s16(mulhi(x[1],OC_C1S7),mulhi(x[7],OC_C7S1));
  r=vaddq_s16(t[4],t[5]);t[5]=mulhi(vsubq_s16(t[4],t[5]),OC_C4S4);t[4]=r;
  r=vaddq_s16(t[7],t[6]);t[6]=mulhi(vsubq_s16(t[7],t[6]),OC_C4S4);t[7]=r;
  r=vaddq_s16(t[0],t[3]);t[3]=vsubq_s16(t[0],t[3]);t[0]=r;
  r=vaddq_s16(t[1],t[2]);t[2]=vsubq_s16(t[1],t[2]);t[1]=r;
  r=vaddq_s16(t[6],t[5]);t[5]=vsubq_s16(t[6],t[5]);t[6]=r;
  x[0]=vaddq_s16(t[0],t[7]);x[7]=vsubq_s16(t[0],t[7]);
  x[1]=vaddq_s16(t[1],t[6]);x[6]=vsubq_s16(t[1],t[6]);
  x[2]=vaddq_s16(t[2],t[5]);x[5]=vsubq_s16(t[2],t[5]);
  x[3]=vaddq_s16(t[3],t[4]);x[4]=vsubq_s16(t[3],t[4]);
}
void adv_theora_idct(ogg_int16_t y[64],ogg_int16_t x[64],int last) {
  int16x8_t rows[8];
  (void)last;
  for(int i=0;i<8;i++) rows[i]=vld1q_s16(x+i*8);
  transpose(rows);idct8(rows);transpose(rows);idct8(rows);
  for(int i=0;i<8;i++) {
    vst1q_s16(y+i*8,vrshrq_n_s16(rows[i],4));
    vst1q_s16(x+i*8,vdupq_n_s16(0));
  }
}
static void filter_rows(const oc_theora_state *,signed char [256],int,int,int,int);
void adv_theora_accel_init(th_dec_ctx *decoder) {
  decoder->state.opt_vtable.frag_copy=adv_theora_copy;
  decoder->state.opt_vtable.frag_copy_list=adv_theora_copy_list;
  decoder->state.opt_vtable.frag_recon_intra=adv_theora_intra;
  decoder->state.opt_vtable.frag_recon_inter=adv_theora_inter;
  decoder->state.opt_vtable.frag_recon_inter2=adv_theora_inter2;
  decoder->state.opt_vtable.idct8x8=adv_theora_idct;
  decoder->state.opt_vtable.state_loop_filter_frag_rows=filter_rows;
}

static inline void filter(int16x8_t a,int16x8_t *b,int16x8_t *c,int16x8_t d,int limit) {
  int16x8_t difference=vsubq_s16(*c,*b);
  int16x8_t f=vrshrq_n_s16(vaddq_s16(vsubq_s16(a,d),vmulq_n_s16(difference,3)),3);
  int16x8_t magnitude=vabsq_s16(f);
  magnitude=vmaxq_s16(vdupq_n_s16(0),vminq_s16(magnitude,vsubq_s16(vdupq_n_s16(limit*2),magnitude)));
  f=vbslq_s16(vcltq_s16(f,vdupq_n_s16(0)),vnegq_s16(magnitude),magnitude);
  *b=vaddq_s16(*b,f);*c=vsubq_s16(*c,f);
}
void adv_theora_filter_v(unsigned char *pixels,int stride,int limit) {
  int16x8_t a=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(pixels-stride*2)));
  int16x8_t b=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(pixels-stride)));
  int16x8_t c=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(pixels)));
  int16x8_t d=vreinterpretq_s16_u16(vmovl_u8(vld1_u8(pixels+stride)));
  filter(a,&b,&c,d,limit);
  vst1_u8(pixels-stride,vqmovun_s16(b));vst1_u8(pixels,vqmovun_s16(c));
}
void adv_theora_filter_h(unsigned char *pixels,int stride,int limit) {
  unsigned char gathered[32],pairs[16];
  for(int y=0;y<8;y++) memcpy(gathered+y*4,pixels+y*stride-2,4);
  uint8x8x4_t columns=vld4_u8(gathered);
  int16x8_t a=vreinterpretq_s16_u16(vmovl_u8(columns.val[0]));
  int16x8_t b=vreinterpretq_s16_u16(vmovl_u8(columns.val[1]));
  int16x8_t c=vreinterpretq_s16_u16(vmovl_u8(columns.val[2]));
  int16x8_t d=vreinterpretq_s16_u16(vmovl_u8(columns.val[3]));
  filter(a,&b,&c,d,limit);
  uint8x8x2_t result=vzip_u8(vqmovun_s16(b),vqmovun_s16(c));
  vst1_u8(pairs,result.val[0]);vst1_u8(pairs+8,result.val[1]);
  for(int y=0;y<8;y++) memcpy(pixels+y*stride-1,pairs+y*2,2);
}
/* Preserve Xiph's edge visitation order; intersecting filters do not commute. */
static void filter_rows(const oc_theora_state *state,signed char bounds[256],
 int refi,int plane,int first,int end) {
  (void)bounds;
  const oc_fragment_plane *p=state->fplanes+plane;
  const oc_fragment *frags=state->frags;
  ptrdiff_t top=p->froffset,bottom=top+p->nfrags;
  int stride=state->ref_ystride[plane];
  int limit=state->loop_filter_limits[state->qis[0]];
  unsigned char *frame=state->ref_frame_data[refi];
  for(ptrdiff_t row=top+first*(ptrdiff_t)p->nhfrags;row<top+end*(ptrdiff_t)p->nhfrags;row+=p->nhfrags) {
    for(ptrdiff_t f=row;f<row+p->nhfrags;f++) if(frags[f].coded) {
      unsigned char *ref=frame+state->frag_buf_offs[f];
      if(f>row)adv_theora_filter_h(ref,stride,limit);
      if(row>top)adv_theora_filter_v(ref,stride,limit);
      if(f+1<row+p->nhfrags&&!frags[f+1].coded)adv_theora_filter_h(ref+8,stride,limit);
      if(f+p->nhfrags<bottom&&!frags[f+p->nhfrags].coded)adv_theora_filter_v(ref+stride*8,stride,limit);
    }
  }
}
