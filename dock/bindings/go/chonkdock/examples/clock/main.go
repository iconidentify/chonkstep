// A digital clock tile in stdlib Go — the chonkdock hello-world.
//
// Register it with a .dockapp file whose exec points at the built
// binary (or install the directory with `scripts/chonk-get`), and the
// dock launches, themes, supervises and restarts it.
package main

import (
	"fmt"
	"os"
	"time"

	chonkdock "chonkstep.dev/chonkdock"
)

// 3x5 bitmaps for '0'-'9' and ':' — three bits per row, top first.
var glyphs = map[byte][5]uint8{
	'0': {7, 5, 5, 5, 7}, '1': {2, 6, 2, 2, 7}, '2': {7, 1, 7, 4, 7},
	'3': {7, 1, 7, 1, 7}, '4': {5, 5, 7, 1, 1}, '5': {7, 4, 7, 1, 7},
	'6': {7, 4, 7, 5, 7}, '7': {7, 1, 2, 2, 2}, '8': {7, 5, 7, 5, 7},
	'9': {7, 5, 7, 1, 7}, ':': {0, 2, 0, 2, 0},
}

var (
	bg = [4]byte{22, 24, 30, 255}
	fg = [4]byte{120, 200, 255, 255} // premultiplied RGBA
)

func main() {
	shown := ""
	app := &chonkdock.Dockapp{
		ID:             "go-dockclock",
		RedrawInterval: 250 * time.Millisecond,
		Draw: func(ctx *chonkdock.Ctx, buf []byte) bool {
			hhmm := time.Now().Format("15:04")
			if hhmm == shown {
				return false // nothing changed; send nothing
			}
			px := int(ctx.TilePx) / 24
			if px < 1 {
				px = 1
			}
			for i := 0; i < len(buf); i += 4 {
				copy(buf[i:], bg[:])
			}
			textW := len(hhmm)*4*px - px
			x0 := (int(ctx.TilePx) - textW) / 2
			y0 := (int(ctx.Height) - 5*px) / 2
			for i := 0; i < len(hhmm); i++ {
				drawGlyph(ctx, buf, glyphs[hhmm[i]], x0+i*4*px, y0, px)
			}
			shown = hhmm
			return true
		},
		OnTheme: func(ctx *chonkdock.Ctx) { shown = "" },
	}
	if err := app.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "go-dockclock: %v\n", err)
		os.Exit(1)
	}
}

func drawGlyph(ctx *chonkdock.Ctx, buf []byte, rows [5]uint8, x0, y0, px int) {
	stride := int(ctx.TilePx) * 4
	for row, bits := range rows {
		for col := 0; col < 3; col++ {
			if bits&(4>>col) == 0 {
				continue
			}
			for dy := 0; dy < px; dy++ {
				base := (y0+row*px+dy)*stride + (x0+col*px)*4
				for dx := 0; dx < px; dx++ {
					copy(buf[base+dx*4:], fg[:])
				}
			}
		}
	}
}
