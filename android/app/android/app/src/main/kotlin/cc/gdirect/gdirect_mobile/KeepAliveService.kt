package cc.gdirect.gdirect_mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.os.PowerManager

/** A foreground service that keeps transfers running with the screen off. */
class KeepAliveService : Service() {
    private var wake: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val text = intent?.getStringExtra("text").orEmpty()
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel("transfers", "Transfers", NotificationManager.IMPORTANCE_LOW)
        )
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val open = PendingIntent.getActivity(this, 0, launch, PendingIntent.FLAG_IMMUTABLE)
        val progress = intent?.getIntExtra("progress", -1) ?: -1
        val builder = Notification.Builder(this, "transfers")
            .setContentTitle("gdirect")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentIntent(open)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
        if (progress in 0..100) builder.setProgress(100, progress, false)
        val notification = builder.build()
        startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        if (wake == null) {
            val power = getSystemService(PowerManager::class.java)
            wake = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "gdirect:transfers")
                .apply { acquire(6 * 60 * 60 * 1000L) }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        wake?.let { if (it.isHeld) it.release() }
        wake = null
        super.onDestroy()
    }
}
