package com.example.jms.config;

import com.example.jms.provider.H2JmsConnectionFactory;
import jakarta.jms.ConnectionFactory;
import jakarta.jms.Session;
import org.mybatis.spring.annotation.MapperScan;
import org.springframework.context.annotation.Bean;
import org.springframework.context.annotation.Configuration;
import org.springframework.jms.annotation.EnableJms;
import org.springframework.jms.config.DefaultJmsListenerContainerFactory;
import org.springframework.jms.core.JmsTemplate;

import javax.sql.DataSource;

/**
 * Spring JMS および MyBatis の統合設定クラス。
 * 標準の @EnableJms, ConnectionFactory, JmsTemplate, DefaultJmsListenerContainerFactory を構成します。
 */
@Configuration
@EnableJms
@MapperScan("com.example.jms.mapper")
public class JmsAndDbConfig {

    @Bean
    public ConnectionFactory connectionFactory(DataSource dataSource) {
        return new H2JmsConnectionFactory(dataSource);
    }

    @Bean
    public JmsTemplate jmsTemplate(ConnectionFactory connectionFactory) {
        JmsTemplate template = new JmsTemplate(connectionFactory);
        template.setReceiveTimeout(2000);
        return template;
    }

    @Bean
    public DefaultJmsListenerContainerFactory jmsListenerContainerFactory(ConnectionFactory connectionFactory) {
        DefaultJmsListenerContainerFactory factory = new DefaultJmsListenerContainerFactory();
        factory.setConnectionFactory(connectionFactory);
        factory.setConcurrency("1-1");
        factory.setSessionAcknowledgeMode(Session.AUTO_ACKNOWLEDGE);
        return factory;
    }
}
