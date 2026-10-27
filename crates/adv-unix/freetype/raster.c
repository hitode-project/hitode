/* FreeType 2.13.2's i386 scan converter. Its conic subdivision differs
 * from the DDA selected on 64-bit hosts, including individual coverage bytes.
 * Build the standalone raster with FreeType's normal error numbers and
 * per-library storage. The three upstream source/header files are unchanged. */
#include <stdlib.h>
#define FT_INT64 long long
typedef signed long long FT_Int64;
typedef unsigned long long FT_UInt64;
#define FALL_THROUGH ((void)0)
#define STANDALONE_
#define ft_grays_raster hitode_grays_raster
#include "ftgrays-i386.c"
