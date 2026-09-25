package com.example.jms.listener;

import jakarta.jms.Message;
import jakarta.jms.TextMessage;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jms.annotation.JmsListener;
import org.springframework.stereotype.Component;

import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;
import java.util.concurrent.CountDownLatch;

/**
 * Spring @JmsListener による非同期メッセージコンシューマ。
 * order_events_queue からメッセージをサブスクライブして処理します。
 */
@Component
public class OrderEventListener {
    private static final Logger log = LoggerFactory.getLogger(OrderEventListener.class);

    private final List<String> receivedMessages = new CopyOnWriteArrayList<>();
    private CountDownLatch latch = new CountDownLatch(1);

    @JmsListener(destination = "order_events_queue")
    public void onOrderEvent(Message message) {
        try {
            if (message instanceof TextMessage) {
                String text = ((TextMessage) message).getText();
                log.info(">>> [@JmsListener] Received order event: {}", text);
                receivedMessages.add(text);
                latch.countDown();
            } else {
                log.info(">>> [@JmsListener] Received unknown message type: {}", message);
            }
        } catch (Exception e) {
            log.error("Error processing JMS message: {}", e.getMessage(), e);
        }
    }

    public List<String> getReceivedMessages() {
        return receivedMessages;
    }

    public void resetLatch(int count) {
        this.latch = new CountDownLatch(count);
    }

    public CountDownLatch getLatch() {
        return latch;
    }
}
