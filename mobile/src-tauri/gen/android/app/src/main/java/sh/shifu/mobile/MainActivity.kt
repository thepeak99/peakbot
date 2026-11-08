package sh.shifu.mobile

import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

// Android 17 LNP: LAN dials silently time out without this; loopback is exempt.
private const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // targetSdk 35+ enforces edge-to-edge (no opt-out from 36), and Android
    // WebView reports env(safe-area-inset-*) as 0 — so the SPA's top bar
    // and the Profiles page drew under the status bar. Keep the app edge to
    // edge but inset the content view by the system bars, display cutout
    // and the keyboard: the WebView then lies entirely inside the safe
    // area (its own safe-area insets correctly stay 0, so nothing is
    // padded twice). The window background (theme) is the page colour, so
    // the inset strips match the page.
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val safe = insets.getInsets(
        WindowInsetsCompat.Type.systemBars() or
          WindowInsetsCompat.Type.displayCutout() or
          WindowInsetsCompat.Type.ime()
      )
      view.setPadding(safe.left, safe.top, safe.right, safe.bottom)
      WindowInsetsCompat.CONSUMED
    }
    if (Build.VERSION.SDK_INT >= 37 &&
        checkSelfPermission(LOCAL_NETWORK) != PackageManager.PERMISSION_GRANTED) {
      requestPermissions(arrayOf(LOCAL_NETWORK), 0x4C4E)
    }
  }
}
