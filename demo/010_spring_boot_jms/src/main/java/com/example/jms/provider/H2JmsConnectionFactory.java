package com.example.jms.provider;

import jakarta.jms.Connection;
import jakarta.jms.ConnectionFactory;
import jakarta.jms.JMSContext;
import jakarta.jms.JMSException;

import javax.sql.DataSource;

/**
 * H2 Database Rust 用 Jakarta JMS ConnectionFactory 実装。
 * Spring Boot の DataSource を注入して利用します。
 */
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
        throw new UnsupportedOperationException("JMSContext is not implemented; use standard JMS Connection/Session");
    }

    @Override
    public JMSContext createContext(String userName, String password) {
        throw new UnsupportedOperationException("JMSContext is not implemented");
    }

    @Override
    public JMSContext createContext(String userName, String password, int sessionMode) {
        throw new UnsupportedOperationException("JMSContext is not implemented");
    }

    @Override
    public JMSContext createContext(int sessionMode) {
        throw new UnsupportedOperationException("JMSContext is not implemented");
    }
}
