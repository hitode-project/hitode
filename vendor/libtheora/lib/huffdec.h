/********************************************************************
 *                                                                  *
 * THIS FILE IS PART OF THE OggTheora SOFTWARE CODEC SOURCE CODE.   *
 * USE, DISTRIBUTION AND REPRODUCTION OF THIS LIBRARY SOURCE IS     *
 * GOVERNED BY A BSD-STYLE SOURCE LICENSE INCLUDED WITH THIS SOURCE *
 * IN 'COPYING'. PLEASE READ THESE TERMS BEFORE DISTRIBUTING.       *
 *                                                                  *
 * THE Theora SOURCE CODE IS COPYRIGHT (C) 2002-2009                *
 * by the Xiph.Org Foundation and contributors                      *
 * https://www.xiph.org/                                            *
 *                                                                  *
 ********************************************************************

  function:

 ********************************************************************/

#if !defined(_huffdec_H)
# define _huffdec_H (1)
# include <stdint.h>
# include "huffman.h"
# include "bitpack.h"



int oc_huff_trees_unpack(oc_pack_buf *_opb,
 ogg_int16_t *_nodes[TH_NHUFFMAN_TABLES]);
int oc_huff_trees_copy(ogg_int16_t *_dst[TH_NHUFFMAN_TABLES],
 const ogg_int16_t *const _src[TH_NHUFFMAN_TABLES]);
void oc_huff_trees_clear(ogg_int16_t *_nodes[TH_NHUFFMAN_TABLES]);
int oc_huff_token_decode_c(oc_pack_buf *_opb,const ogg_int16_t *_node);

/*The primary table flattens paths that fit in nine bits of lookahead.
  Each entry stores the reference lookahead, consumed bits, and token. */
#define ADV_HUFF_BITS (9)
typedef struct {
  const ogg_int16_t *tree;
  uint16_t entries[1<<ADV_HUFF_BITS];
} adv_huff_table;

void adv_huff_table_init(adv_huff_table *_table,const ogg_int16_t *_tree);
int adv_huff_token_decode_slow(oc_pack_buf *_opb,const adv_huff_table *_table);

/*Append exactly the same whole bytes as the scalar reservoir refill.
  The caller handles short tails with the matching scalar EOF rules. */
static inline int adv_pack_refill64(oc_pack_buf *_opb){
  if(OC_PB_WINDOW_SIZE!=64||_opb->stop-_opb->ptr<8)return 0;
  const unsigned char *p=_opb->ptr;
  uint64_t word=(uint64_t)p[0]<<56|(uint64_t)p[1]<<48|
   (uint64_t)p[2]<<40|(uint64_t)p[3]<<32|(uint64_t)p[4]<<24|
   (uint64_t)p[5]<<16|(uint64_t)p[6]<<8|p[7];
  int room=64-_opb->bits;
  int tail=room&7;
  _opb->window|=(word>>_opb->bits>>tail)<<tail;
  _opb->ptr+=room>>3;
  _opb->bits=64-tail;
  return 1;
}

/*The residual reader requests 4-bit selectors and up to 12 extra bits. */
static inline unsigned adv_pack_read(oc_pack_buf *_opb,int _bits){
  if(OC_PB_WINDOW_SIZE!=64||(_opb->bits<_bits&&!adv_pack_refill64(_opb))){
    return (unsigned)oc_pack_read_c(_opb,_bits);
  }
  unsigned value=(uint64_t)_opb->window>>(64-_bits);
  _opb->window<<=_bits;
  _opb->bits-=_bits;
  return value;
}

static inline int adv_huff_token_decode(oc_pack_buf *_opb,
 const adv_huff_table *_table){
  if(OC_PB_WINDOW_SIZE==64){
    unsigned entry=_table->entries[(uint64_t)_opb->window>>(64-ADV_HUFF_BITS)];
    int peek=entry>>12;
    if(peek<=ADV_HUFF_BITS&&peek<=_opb->bits){
      int used=entry>>8&15;
      _opb->window<<=used;
      _opb->bits-=used;
      return entry&255;
    }
  }
  return adv_huff_token_decode_slow(_opb,_table);
}

#endif
