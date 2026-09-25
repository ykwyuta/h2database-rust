package com.example.jms.provider;

import jakarta.jms.JMSException;
import jakarta.jms.Queue;

public class H2JmsQueue implements Queue {
    private final String queueName;

    public H2JmsQueue(String queueName) {
        this.queueName = queueName;
    }

    @Override
    public String getQueueName() throws JMSException {
        return queueName;
    }

    @Override
    public String toString() {
        return queueName;
    }
}
