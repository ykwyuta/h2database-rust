package com.example.jms.provider;

import jakarta.jms.*;

import javax.sql.DataSource;

public class H2JmsConnection implements Connection {
    private final DataSource dataSource;
    private String clientID;
    private ExceptionListener exceptionListener;
    private boolean started = false;
    private boolean closed = false;

    public H2JmsConnection(DataSource dataSource) {
        this.dataSource = dataSource;
    }

    @Override
    public Session createSession(boolean transacted, int acknowledgeMode) throws JMSException {
        return new H2JmsSession(dataSource, transacted, acknowledgeMode);
    }

    @Override
    public Session createSession(int sessionMode) throws JMSException {
        return createSession(sessionMode == Session.SESSION_TRANSACTED, sessionMode);
    }

    @Override
    public Session createSession() throws JMSException {
        return createSession(false, Session.AUTO_ACKNOWLEDGE);
    }

    @Override
    public String getClientID() throws JMSException {
        return clientID;
    }

    @Override
    public void setClientID(String clientID) throws JMSException {
        this.clientID = clientID;
    }

    @Override
    public ConnectionMetaData getMetaData() throws JMSException {
        return null;
    }

    @Override
    public ExceptionListener getExceptionListener() throws JMSException {
        return exceptionListener;
    }

    @Override
    public void setExceptionListener(ExceptionListener listener) throws JMSException {
        this.exceptionListener = listener;
    }

    @Override
    public void start() throws JMSException {
        this.started = true;
    }

    @Override
    public void stop() throws JMSException {
        this.started = false;
    }

    @Override
    public void close() throws JMSException {
        this.closed = true;
    }

    @Override
    public ConnectionConsumer createConnectionConsumer(Destination destination, String messageSelector, ServerSessionPool sessionPool, int maxMessages) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public ConnectionConsumer createSharedConnectionConsumer(Topic topic, String subscriptionName, String messageSelector, ServerSessionPool sessionPool, int maxMessages) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public ConnectionConsumer createDurableConnectionConsumer(Topic topic, String subscriptionName, String messageSelector, ServerSessionPool sessionPool, int maxMessages) throws JMSException {
        throw new UnsupportedOperationException();
    }

    @Override
    public ConnectionConsumer createSharedDurableConnectionConsumer(Topic topic, String subscriptionName, String messageSelector, ServerSessionPool sessionPool, int maxMessages) throws JMSException {
        throw new UnsupportedOperationException();
    }
}
