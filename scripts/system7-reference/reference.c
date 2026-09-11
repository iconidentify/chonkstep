/* ChonkStep's reference fixture, running inside genuine System 7.
 * All chrome is drawn by the OS Window Manager; this app paints content only.
 * Build with Retro68's freely implemented Multiversal Interfaces.
 */
#include <Quickdraw.h>
#include <Windows.h>
#include <Fonts.h>
#include <Menus.h>
#include <TextEdit.h>
#include <Dialogs.h>
#include <Events.h>
#include <OSUtils.h>
#include <string.h>

// Multiversal Interfaces omit this standard Window Manager variant.
// Inside Macintosh, Toolbox Essentials, Types of Windows: zoomDocProc = 8.
enum { kReferenceZoomDocProc = 8 };

static WindowPtr specimen;
static WindowPtr other;
static short proc = kReferenceZoomDocProc;
static unsigned char title[256];
static short glyph_page;

static void choose_title(short which) {
    static const char *names[] = {
        "Terminal", "A deliberately long document title that reaches beyond the available title bar width",
        "", "Caf\216 - na\225ve - \201ngstr\232m"
    };
    const char *text = names[which];
    title[0] = strlen(text);
    memcpy(title + 1, text, title[0]);
    if (specimen) SetWTitle(specimen, title);
}

static void create_specimen(void) {
    Rect content = {140, 120, 380, 520};
    if (specimen) DisposeWindow(specimen);
    specimen = NewWindow(0, &content, title, true, proc, (WindowPtr)-1, true, 0);
}

static void draw_content(WindowPtr window) {
    short index;
    SetPort(window);
    BeginUpdate(window);
    EraseRect(&window->portRect);
    TextFont(0);
    TextSize(12);
    TextFace(0);
    if (window == specimen) {
        /* Separate characters in fixed cells so each glyph and advance can
         * be measured from the capture. No font resources are exported. */
        for (index = 0; index < (glyph_page == 0 ? 95 : glyph_page == 1 ? 96 : 32); ++index) {
            unsigned char character = index + (glyph_page == 0 ? 32 : glyph_page == 1 ? 128 : 224);
            short x = 12 + (index % 16) * 24;
            short baseline = 24 + (index / 16) * 28;
            short advance = CharWidth(character);
            MoveTo(x, baseline);
            DrawChar(character);
            /* An independent pixel ruler for QuickDraw's logical advance. */
            if (advance > 0) {
                MoveTo(x, baseline + 5);
                LineTo(x + advance - 1, baseline + 5);
            }
        }
    }
    EndUpdate(window);
}

static void key(unsigned char character) {
    switch (character) {
        case '1': case '2': case '3': case '4': choose_title(character - '1'); break;
        case 'd': proc = documentProc; create_specimen(); break;
        case 'z': proc = kReferenceZoomDocProc; create_specimen(); break;
        case 'r': create_specimen(); break;
        case 'e': MoveWindow(specimen, 622, 525, true); break;
        case 'g':
            glyph_page = (glyph_page + 1) % 3;
            SetPort(specimen);
            InvalRect(&specimen->portRect);
            break;
        case 'h': HideCursor(); break;
        case 'q': ExitToShell(); break;
    }
}

int main(void) {
    EventRecord event;
    Rect background = {180, 620, 330, 900};
    WindowPtr window;
    InitGraf(&qd.thePort);
    InitFonts();
    InitWindows();
    InitMenus();
    TEInit();
    InitDialogs(0);
    InitCursor();
    DrawMenuBar();
    other = NewWindow(0, &background, "\pOther", true, documentProc, (WindowPtr)-1, true, 0);
    choose_title(0);
    create_specimen();
    for (;;) {
        if (!WaitNextEvent(everyEvent, &event, 1, 0)) continue;
        if (event.what == updateEvt) draw_content((WindowPtr)event.message);
        if (event.what == keyDown) key(event.message & charCodeMask);
        if (event.what != mouseDown) continue;
        switch (FindWindow(event.where, &window)) {
            case inContent: SelectWindow(window); break;
            case inDrag: DragWindow(window, event.where, &qd.screenBits.bounds); break;
            case inGoAway:
                /* Preserve the specimen for repeatable held-button captures. */
                TrackGoAway(window, event.where);
                break;
            case inZoomIn: case inZoomOut: {
                short part = FindWindow(event.where, &window);
                if (TrackBox(window, event.where, part)) ZoomWindow(window, part, true);
                break;
            }
            case inSysWindow: SystemClick(&event, window); break;
            case inMenuBar: MenuSelect(event.where); HiliteMenu(0); break;
        }
    }
}
