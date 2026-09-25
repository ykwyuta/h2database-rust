package com.example.jms.provider;

import jakarta.jms.Connection;
import jakarta.jms.ConnectionFactory;
import jakarta.jms.JMSContext;
import jakarta.jms.JMSException;

import javax.sql.DataSource;

public class H2JmsConnectionFactory implements ConnectionFactory {
    private final DataSource dataSource;

    public H2JmsConnectionFactory(DataSource dataSource) {
        this.dataSource = dataSource;
    }

    @Override
    public Connection createConnection() throws JMSException {
        return new H2JmsConnection(dataSource);
    }

    @Override
    public Connection createConnection(String userName, String password) throws JMSException {
        return createConnection();
    }

    @Override
    public JMSContext createContext() {
        throw new UnsupportedOperationException();
    }

    @Override
    public JMSContext createContext(String userName, String password) {
        throw new UnsupportedOperationException();
    }

    @Override
    public JMSContext createContext(String userName, String password, int sessionMode) {
        throw new UnsupportedOperationException();
    }

    @Override
    public JMSContext createContext(int sessionMode) {
        throw new UnsupportedOperationException();
    }
}
