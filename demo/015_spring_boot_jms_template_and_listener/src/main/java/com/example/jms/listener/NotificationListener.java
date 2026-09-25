package com.example.jms.listener;

import com.example.jms.model.NotificationMessage;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jms.annotation.JmsListener;
import org.springframework.messaging.handler.annotation.Header;
import org.springframework.messaging.handler.annotation.Payload;
import org.springframework.stereotype.Component;

import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;

/**
 * Spring @JmsListener による非同期メッセージ購読リスナー。
 * @Payload および @Header によるメッセージ本体とプロパティの自動注入を実演します。
 */
@Component
public class NotificationListener {
    private static final Logger log = LoggerFactory.getLogger(NotificationListener.class);

    private final List<NotificationMessage> receivedNotifications = new CopyOnWriteArrayList<>();
    private CountDownLatch latch = new CountDownLatch(1);

    @JmsListener(destination = "notification_queue")
    public void onNotification(@Payload NotificationMessage notification,
                               @Header(name = "priorityLevel", defaultValue = "NORMAL") String priorityLevel,
                               @Header(name = "sourceApp", defaultValue = "UNKNOWN") String sourceApp) {
        log.info(">>> [@JmsListener] Received Notification: id={}, recipient='{}', content='{}' [Priority: {}, Source: {}]",
                notification.getNotificationId(), notification.getRecipient(), notification.getContent(), priorityLevel, sourceApp);

        receivedNotifications.add(notification);
        latch.countDown();
    }

    public List<NotificationMessage> getReceivedNotifications() {
        return receivedNotifications;
    }

    public CountDownLatch getLatch() {
        return latch;
    }

    public void resetLatch(int count) {
        this.latch = new CountDownLatch(count);
    }
}
