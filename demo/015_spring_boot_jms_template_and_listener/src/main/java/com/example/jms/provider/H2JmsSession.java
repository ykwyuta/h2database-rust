package com.example.jms.provider;

import jakarta.jms.*;

import javax.sql.DataSource;
import java.io.Serializable;

public class H2JmsSession implements Session {
    private final DataSource dataSource;
    private final boolean transacted;
    private final int acknowledgeMode;
    private boolean closed = false;

    public H2JmsSession(DataSource dataSource, boolean transacted, int acknowledgeMode) {
        this.dataSource = dataSource;
        this.transacted = transacted;
        this.acknowledgeMode = acknowledgeMode;
    }

    @Override
    public BytesMessage createBytesMessage() throws JMSException {
        throw new UnsupportedOperationException("BytesMessage not implemented in demo");
    }

    @Override
    public MapMessage createMapMessage() throws JMSException {
        throw new UnsupportedOperationException("MapMessage not implemented in demo");
    }

    @Override
    public Message createMessage() throws JMSException {
        return new H2JmsTextMessage();
    }

    @Override
    public ObjectMessage createObjectMessage() throws JMSException {
        throw new UnsupportedOperationException("ObjectMessage not implemented in demo");
    }

    @Override
    public ObjectMessage createObjectMessage(Serializable object) throws JMSException {
        throw new UnsupportedOperationException("ObjectMessage not implemented in demo");
    }

    @Override
    public StreamMessage createStreamMessage() throws JMSException {
        throw new UnsupportedOperationException("StreamMessage not implemented in demo");
    }

    @Override
    public TextMessage createTextMessage() throws JMSException {
        return new H2JmsTextMessage();
    }

    @Override
    public TextMessage createTextMessage(String text) throws JMSException {
        return new H2JmsTextMessage(text);
    }

    @Override
    public boolean getTransacted() throws JMSException {
        return transacted;
    }

    @Override
    public int getAcknowledgeMode() throws JMSException {
        return acknowledgeMode;
    }

    @Override
    public void commit() throws JMSException {
    }

    @Override
    public void rollback() throws JMSException {
    }

    @Override
    public void close() throws JMSException {
        this.closed = true;
    }

    @Override
    public void recover() throws JMSException {
    }

    @Override
    public MessageListener getMessageListener() throws JMSException {
        return null;
    }

    @Override
    public void setMessageListener(MessageListener listener) throws JMSException {
    }

    @Override
    public void run() {
    }

    @Override
    public MessageProducer createProducer(Destination destination) throws JMSException {
        return new H2JmsMessageProducer(dataSource, destination);
    }

    @Override
    public MessageConsumer createConsumer(Destination destination) throws JMSException {
        return new H2JmsMessageConsumer(dataSource, destination);
    }

    @Override
    public MessageConsumer createConsumer(Destination destination, String messageSelector) throws JMSException {
        return createConsumer(destination);
    }

    @Override
    public MessageConsumer createConsumer(Destination destination, String messageSelector, boolean noLocal) throws JMSException {
        return createConsumer(destination);
    }

    @Override
    public MessageConsumer createSharedConsumer(Topic topic, String sharedSubscriptionName) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public MessageConsumer createSharedConsumer(Topic topic, String sharedSubscriptionName, String messageSelector) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public Queue createQueue(String queueName) throws JMSException {
        return new H2JmsQueue(queueName);
    }

    @Override
    public Topic createTopic(String topicName) throws JMSException {
        throw new UnsupportedOperationException("Topic is not supported");
    }

    @Override
    public TopicSubscriber createDurableSubscriber(Topic topic, String name) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public TopicSubscriber createDurableSubscriber(Topic topic, String name, String messageSelector, boolean noLocal) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public MessageConsumer createDurableConsumer(Topic topic, String name) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public MessageConsumer createDurableConsumer(Topic topic, String name, String messageSelector, boolean noLocal) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public MessageConsumer createSharedDurableConsumer(Topic topic, String name) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public MessageConsumer createSharedDurableConsumer(Topic topic, String name, String messageSelector) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public QueueBrowser createBrowser(Queue queue) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public QueueBrowser createBrowser(Queue queue, String messageSelector) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public TemporaryQueue createTemporaryQueue() throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public TemporaryTopic createTemporaryTopic() throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public void unsubscribe(String name) throws JMSException {
    }
}
