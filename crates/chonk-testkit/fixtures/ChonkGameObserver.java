/* Read-only observer for an externally supplied, unmodified Chonkcraft jar.
 * No Robot, synthetic AWT event dispatch, or game-action test hooks are used.
 * Run only in a private nested session. This adds overhead and is NOT a FPS
 * benchmark. The licensed game assets are never part of this repository.
 */
import java.awt.AWTEvent;
import java.awt.Component;
import java.awt.Container;
import java.awt.EventQueue;
import java.awt.Frame;
import java.awt.Point;
import java.awt.Rectangle;
import java.awt.Toolkit;
import java.awt.event.KeyEvent;
import java.awt.event.MouseEvent;
import java.lang.reflect.Method;
import java.util.List;
import javax.swing.JFrame;
import javax.swing.SwingUtilities;
import javax.swing.Timer;

public final class ChonkGameObserver {
    private static String previous = "";
    private static long sequence;

    private ChonkGameObserver() {}

    public static void main(String[] args) throws Exception {
        // The real entry point must choose its Java2D pipeline before the
        // observer initializes AWT. Menu construction is queued by the game.
        Class.forName("net.chonkbase.chonkcraft.desktop.Main")
                .getMethod("main", String[].class).invoke(null, (Object) args);
        EventQueue.invokeLater(() -> {
            Toolkit.getDefaultToolkit().addAWTEventListener(event -> {
                if (event instanceof MouseEvent mouse) {
                    if (mouse.getID() == MouseEvent.MOUSE_PRESSED
                            || mouse.getID() == MouseEvent.MOUSE_RELEASED
                            || mouse.getID() == MouseEvent.MOUSE_DRAGGED) {
                        JFrame frame = frameFor(mouse.getComponent());
                        if (frame != null) {
                            Point point = SwingUtilities.convertPoint(mouse.getComponent(),
                                    mouse.getPoint(), frame.getContentPane());
                            emit("{\"event\":\"mouse\",\"id\":" + mouse.getID()
                                    + ",\"button\":" + mouse.getButton()
                                    + ",\"x\":" + point.x + ",\"y\":" + point.y
                                    + ",\"screen_x\":" + mouse.getXOnScreen()
                                    + ",\"screen_y\":" + mouse.getYOnScreen() + "}");
                        }
                    }
                } else if (event instanceof KeyEvent key) {
                    if (key.getID() == KeyEvent.KEY_PRESSED || key.getID() == KeyEvent.KEY_RELEASED) {
                        emit("{\"event\":\"key\",\"id\":" + key.getID()
                                + ",\"code\":" + key.getKeyCode() + "}");
                    }
                }
                // Snapshot after the game's listener consumes this real event.
                EventQueue.invokeLater(ChonkGameObserver::snapshot);
            }, AWTEvent.MOUSE_EVENT_MASK | AWTEvent.MOUSE_MOTION_EVENT_MASK | AWTEvent.KEY_EVENT_MASK);
            new Timer(200, event -> snapshot()).start();
            snapshot();
        });
    }

    private static JFrame frameFor(Component component) {
        var window = SwingUtilities.getWindowAncestor(component);
        return window instanceof JFrame frame ? frame : null;
    }

    private static void emit(String json) {
        System.out.println("CHONK_OBSERVER " + (++sequence) + " " + json);
    }

    private static Object readHook(Container content, String name) throws ReflectiveOperationException {
        Method method = content.getClass().getDeclaredMethod(name);
        method.setAccessible(true);
        return method.invoke(content);
    }

    private static String quote(String value) {
        StringBuilder result = new StringBuilder("\"");
        for (int index = 0; index < value.length(); index++) {
            char c = value.charAt(index);
            if (c == '"' || c == '\\') result.append('\\').append(c);
            else if (c < 32) result.append(String.format("\\u%04x", (int) c));
            else result.append(c);
        }
        return result.append('"').toString();
    }

    private static void snapshot() {
        for (Frame candidate : Frame.getFrames()) {
            if (!(candidate instanceof JFrame frame) || !frame.isShowing()) continue;
            Container content = frame.getContentPane();
            if (!content.isShowing() || content.getWidth() == 0 || content.getHeight() == 0) continue;
            Point at = content.getLocationOnScreen();
            var transform = frame.getGraphicsConfiguration().getDefaultTransform();
            StringBuilder json = new StringBuilder("{\"event\":\"snapshot\",\"class\":")
                    .append(quote(content.getClass().getName()))
                    .append(",\"frame_x\":").append(frame.getX()).append(",\"frame_y\":").append(frame.getY())
                    .append(",\"frame_w\":").append(frame.getWidth()).append(",\"frame_h\":").append(frame.getHeight())
                    .append(",\"screen_x\":").append(at.x).append(",\"screen_y\":").append(at.y)
                    .append(",\"w\":").append(content.getWidth()).append(",\"h\":").append(content.getHeight())
                    .append(",\"scale_x\":").append(transform.getScaleX())
                    .append(",\"scale_y\":").append(transform.getScaleY())
                    .append(",\"focused\":").append(frame.isFocused()).append(",\"entries\":[");
            if (content.getClass().getName().equals("net.chonkbase.chonkcraft.desktop.MenuScreen")) {
                try {
                    // Read-only hooks describe the actual painted menu. Never
                    // call pressForTest or any show* hook to activate a page.
                    List<?> captions = (List<?>) readHook(content, "captionsForTest");
                    List<?> bounds = (List<?>) readHook(content, "slotBoundsForTest");
                    if (captions.size() == bounds.size()) {
                        double scale = Math.min(content.getWidth() / 640.0, content.getHeight() / 480.0);
                        int width = (int) Math.round(640 * scale);
                        int height = (int) Math.round(480 * scale);
                        int left = (content.getWidth() - width) / 2;
                        int top = (content.getHeight() - height) / 2;
                        for (int index = 0; index < captions.size(); index++) {
                            Rectangle rect = (Rectangle) bounds.get(index);
                            if (index > 0) json.append(',');
                            json.append("{\"caption\":").append(quote((String) captions.get(index)))
                                    .append(",\"x\":").append(left + Math.round(rect.getCenterX() * width / 640.0))
                                    .append(",\"y\":").append(top + Math.round(rect.getCenterY() * height / 480.0))
                                    .append('}');
                        }
                    }
                } catch (ReflectiveOperationException error) {
                    throw new IllegalStateException("unsupported game observer hooks", error);
                }
            }
            String value = json.append("]}").toString();
            if (!value.equals(previous)) {
                previous = value;
                emit(value);
            }
        }
    }
}
