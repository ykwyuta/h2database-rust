package com.example.jms.listener;

import com.example.jms.model.PaymentRequest;
import com.example.jms.model.PaymentResponse;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;
import org.springframework.jms.annotation.JmsListener;
import org.springframework.messaging.handler.annotation.SendTo;
import org.springframework.stereotype.Component;

import java.util.UUID;
import java.util.concurrent.CountDownLatch;

/**
 * Spring @JmsListener と @SendTo による Request-Reply (RPC) リスナー。
 * payment_request_queue から PaymentRequest を受信し、
 * 戻り値の PaymentResponse を自動的に payment_reply_queue へ送信します。
 */
@Component
public class PaymentProcessorListener {
    private static final Logger log = LoggerFactory.getLogger(PaymentProcessorListener.class);

    private CountDownLatch latch = new CountDownLatch(1);
    private PaymentRequest lastReceivedRequest;

    @JmsListener(destination = "payment_request_queue")
    @SendTo("payment_reply_queue")
    public PaymentResponse processPayment(PaymentRequest request) {
        log.info(">>> [@JmsListener] Received PaymentRequest: {}", request);
        this.lastReceivedRequest = request;

        boolean approved = request.getAmount() <= 50000.0;
        String status = approved ? "SUCCESS" : "REJECTED_LIMIT_EXCEEDED";
        String txCode = "TX-" + UUID.randomUUID().toString().substring(0, 8).toUpperCase();
        String message = approved ? "Payment authorized successfully" : "Payment amount exceeds limit";

        PaymentResponse response = new PaymentResponse(request.getPaymentId(), status, txCode, message);
        log.info("<<< [@SendTo] Sending auto-reply PaymentResponse to 'payment_reply_queue': {}", response);

        latch.countDown();
        return response;
    }

    public CountDownLatch getLatch() {
        return latch;
    }

    public void resetLatch(int count) {
        this.latch = new CountDownLatch(count);
    }

    public PaymentRequest getLastReceivedRequest() {
        return lastReceivedRequest;
    }
}
